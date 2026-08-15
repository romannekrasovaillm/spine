//! Парсер mermaid-подмножества в AST ([`super::model`]).
//!
//! Flowchart: `graph|flowchart TD|TB|BT|LR|RL`, узлы `A[label]`/`B(label)`/
//! `C{label}`/`D((label))`, рёбра `-->`, `---`, `-.->`, `-- метка -->`,
//! цепочки `A --> B --> C`. Sequence: `participant X as Label`, `->>`, `-->>`,
//! `Note left of|right of X: текст`. Комментарии `%%` и пустые строки
//! игнорируются; известные неподдерживаемые конструкции (`subgraph`, `style`,
//! `click`, `loop`, …) пропускаются с предупреждением.

use std::collections::HashMap;

use crate::error::{HarnessError, Result};

use super::model::{
    Direction, FlowAst, FlowEdge, FlowNode, NoteSide, Participant, SeqAst, SeqItem, SeqMessage,
    SeqNote, Shape, Skipped,
};

/// Формирует [`HarnessError::Mermaid`] с номером строки и фрагментом.
fn err_at(line: usize, text: &str, msg: &str) -> HarnessError {
    let snippet: String = text.chars().take(40).collect();
    HarnessError::Mermaid(format!("строка {line}: {msg}: «{snippet}»"))
}

/// Отрезает комментарий `%%` (вне двойных кавычек).
fn strip_comment(line: &str) -> &str {
    let mut in_quotes = false;
    let mut prev = '\0';
    for (i, c) in line.char_indices() {
        if c == '"' {
            in_quotes = !in_quotes;
        } else if c == '%' && prev == '%' && !in_quotes {
            // `%` — ASCII, поэтому `i - 1` — граница char.
            return &line[..i - 1];
        }
        prev = c;
    }
    line
}

/// Снимает обрамляющие двойные кавычки и обрезает пробелы.
fn unquote(raw: &str) -> String {
    let t = raw.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        t[1..t.len() - 1].to_owned()
    } else {
        t.to_owned()
    }
}

/// Ищет подстроку вне двойных кавычек (байтовый индекс).
fn find_outside_quotes(hay: &str, needle: &str) -> Option<usize> {
    let mut in_quotes = false;
    let mut i = 0;
    while i < hay.len() {
        let s = &hay[i..];
        let c = s.chars().next()?;
        if c == '"' {
            in_quotes = !in_quotes;
            i += 1;
        } else {
            if !in_quotes && s.starts_with(needle) {
                return Some(i);
            }
            i += c.len_utf8();
        }
    }
    None
}

/// Курсор по строке (байтовая позиция, двигается только по char-границам).
struct Cursor<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, pos: 0 }
    }

    /// Остаток строки от текущей позиции.
    fn rest(&self) -> &'a str {
        &self.text[self.pos..]
    }

    /// Пропускает пробельные символы.
    fn skip_ws(&mut self) {
        while let Some(c) = self.rest().chars().next() {
            if c.is_whitespace() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }

    /// Съедает префикс, если он есть.
    fn eat(&mut self, prefix: &str) -> bool {
        if self.rest().starts_with(prefix) {
            self.pos += prefix.len();
            true
        } else {
            false
        }
    }
}

/// Сканирует идентификатор (буквы/цифры/`_`).
fn scan_id(cur: &mut Cursor<'_>, line_no: usize, full: &str) -> Result<String> {
    cur.skip_ws();
    let start = cur.pos;
    while let Some(c) = cur.rest().chars().next() {
        if c.is_alphanumeric() || c == '_' {
            cur.pos += c.len_utf8();
        } else {
            break;
        }
    }
    if cur.pos == start {
        return Err(err_at(line_no, full, "ожидался идентификатор"));
    }
    Ok(cur.text[start..cur.pos].to_owned())
}

/// Распознанная в потоке ссылка на узел flowchart.
struct NodeRef {
    id: String,
    shape: Option<Shape>,
    label: Option<String>,
}

/// Сканирует узел: `id`, `id[label]`, `id(label)`, `id{label}`, `id((label))`.
fn scan_node(cur: &mut Cursor<'_>, line_no: usize, full: &str) -> Result<NodeRef> {
    let id = scan_id(cur, line_no, full)?;
    let (shape, label) = if cur.rest().starts_with("((") {
        cur.pos += 2;
        (Some(Shape::Circle), Some(scan_label_until(cur, "))", line_no, full)?))
    } else if cur.eat("[") {
        (Some(Shape::Rect), Some(scan_label_until(cur, "]", line_no, full)?))
    } else if cur.eat("(") {
        (Some(Shape::Rounded), Some(scan_label_until(cur, ")", line_no, full)?))
    } else if cur.eat("{") {
        (Some(Shape::Rhombus), Some(scan_label_until(cur, "}", line_no, full)?))
    } else {
        (None, None)
    };
    Ok(NodeRef { id, shape, label })
}

/// Сканирует метку до закрывающего токена (внутри кавычек токен не ищется).
fn scan_label_until(cur: &mut Cursor<'_>, close: &str, line_no: usize, full: &str) -> Result<String> {
    let start = cur.pos;
    let mut in_quotes = false;
    loop {
        let Some(c) = cur.rest().chars().next() else {
            return Err(err_at(line_no, full, "незакрытая метка узла"));
        };
        if !in_quotes && cur.rest().starts_with(close) {
            let raw = &cur.text[start..cur.pos];
            cur.pos += close.len();
            return Ok(unquote(raw));
        }
        if c == '"' {
            in_quotes = !in_quotes;
        }
        cur.pos += c.len_utf8();
    }
}

/// Сканирует оператор связи: `-->`, `-.->`, `---`, `-- метка -->`,
/// pipe-метки `-->|метка|` / `---|метка|` / `-.->|метка|`.
/// Возвращает `(метка, plain)`: `plain = true` для линии без стрелки.
fn scan_edge_op(cur: &mut Cursor<'_>, line_no: usize, full: &str) -> Result<(Option<String>, bool)> {
    let mut label = None;
    let plain = if cur.eat("-.->") {
        false
    } else if cur.eat("-->") {
        false
    } else if cur.eat("---") {
        true
    } else if cur.eat("--") {
        let Some(idx) = find_outside_quotes(cur.rest(), "--") else {
            return Err(err_at(line_no, full, "ожидалось '-->' после метки ребра"));
        };
        let raw = unquote(&cur.rest()[..idx]);
        cur.pos += idx;
        let plain = if cur.eat("-->") {
            false
        } else if cur.eat("---") {
            true
        } else {
            return Err(err_at(line_no, full, "ожидалось '-->' или '---' после метки ребра"));
        };
        if !raw.is_empty() {
            label = Some(raw);
        }
        plain
    } else {
        return Err(err_at(line_no, full, "ожидалась связь '-->', '-.->', '---' или '-- метка -->'"));
    };
    // Pipe-метка после оператора: `-->|Да|`, `---|путь|`, `-.->|x|`.
    if cur.eat("|") {
        let Some(idx) = find_outside_quotes(cur.rest(), "|") else {
            return Err(err_at(line_no, full, "незакрытая pipe-метка ребра (ожидалась '|')"));
        };
        let raw = unquote(&cur.rest()[..idx]);
        cur.pos += idx + 1;
        if !raw.is_empty() {
            label = Some(raw);
        }
    }
    Ok((label, plain))
}

/// Регистрирует узел (первое упоминание) или обновляет метку/форму существующего.
fn register_node(nref: NodeRef, nodes: &mut Vec<FlowNode>, ids: &mut HashMap<String, usize>) -> usize {
    if let Some(&i) = ids.get(&nref.id) {
        if let Some(label) = nref.label {
            nodes[i].label = label;
        }
        if let Some(shape) = nref.shape {
            nodes[i].shape = shape;
        }
        return i;
    }
    let i = nodes.len();
    let label = nref.label.unwrap_or_else(|| nref.id.clone());
    nodes.push(FlowNode {
        id: nref.id.clone(),
        label,
        shape: nref.shape.unwrap_or(Shape::Rect),
    });
    ids.insert(nref.id, i);
    i
}

/// Разбирает оператор flowchart: объявление узла или цепочку связей `A --> B --> C`.
fn parse_flow_statement(
    text: &str,
    line_no: usize,
    nodes: &mut Vec<FlowNode>,
    ids: &mut HashMap<String, usize>,
    edges: &mut Vec<FlowEdge>,
) -> Result<()> {
    let mut cur = Cursor::new(text);
    let first = scan_node(&mut cur, line_no, text)?;
    let mut prev = register_node(first, nodes, ids);
    loop {
        cur.skip_ws();
        if cur.rest().is_empty() {
            return Ok(());
        }
        let (label, plain) = scan_edge_op(&mut cur, line_no, text)?;
        let next_ref = scan_node(&mut cur, line_no, text)?;
        let next = register_node(next_ref, nodes, ids);
        edges.push(FlowEdge { from: prev, to: next, label, plain });
        prev = next;
    }
}

/// Первое слово — известная неподдерживаемая конструкция flowchart (пропускаем).
fn is_skippable_flow(text: &str) -> bool {
    let Some(first) = text.split_whitespace().next() else {
        return false;
    };
    matches!(
        first,
        "subgraph" | "end" | "classDef" | "class" | "click" | "style" | "linkStyle" | "direction"
    )
}

/// Разбирает заголовок `graph|flowchart TD|TB|BT|LR|RL`.
fn parse_flow_header(text: &str, line_no: usize) -> Result<Direction> {
    let mut parts = text.split_whitespace();
    let kw = parts.next().unwrap_or("");
    if kw != "graph" && kw != "flowchart" {
        return Err(err_at(line_no, text, "ожидался заголовок 'graph'/'flowchart'"));
    }
    let dir_token = parts.next().ok_or_else(|| {
        err_at(line_no, text, "укажите направление: TD, TB, BT, LR или RL")
    })?;
    match dir_token.to_ascii_uppercase().as_str() {
        "TD" | "TB" => Ok(Direction::TopDown),
        "BT" => Ok(Direction::BottomUp),
        "LR" => Ok(Direction::LeftRight),
        "RL" => Ok(Direction::RightLeft),
        _ => Err(err_at(line_no, text, "неизвестное направление (ожидалось TD|TB|BT|LR|RL)")),
    }
}

/// Разбирает flowchart-диаграмму целиком.
///
/// # Ошибки
/// Некорректный заголовок, битый синтаксис связей/узлов (с номером строки),
/// диаграмма без единого узла.
pub(crate) fn parse_flowchart(input: &str) -> Result<FlowAst> {
    let mut dir: Option<Direction> = None;
    let mut nodes = Vec::new();
    let mut ids = HashMap::new();
    let mut edges = Vec::new();
    let mut skipped = Vec::new();
    for (idx, raw) in input.lines().enumerate() {
        let line_no = idx + 1;
        let text = strip_comment(raw).trim();
        if text.is_empty() {
            continue;
        }
        if dir.is_none() {
            dir = Some(parse_flow_header(text, line_no)?);
            continue;
        }
        if is_skippable_flow(text) {
            skipped.push(Skipped { line: line_no, text: text.to_owned() });
            continue;
        }
        parse_flow_statement(text, line_no, &mut nodes, &mut ids, &mut edges)?;
    }
    let Some(dir) = dir else {
        return Err(HarnessError::Mermaid(
            "пустой ввод: ожидалась mermaid-диаграмма (graph/flowchart ...)".into(),
        ));
    };
    if nodes.is_empty() {
        return Err(HarnessError::Mermaid("диаграмма не содержит ни одного узла".into()));
    }
    Ok(FlowAst { dir, nodes, edges, skipped })
}

/// Разбирает `participant X` / `participant X as Метка` (после ключевого слова).
fn parse_participant(rest: &str, line_no: usize, full: &str) -> Result<(String, Option<String>)> {
    let (id, label) = if let Some(idx) = rest.find(" as ") {
        (rest[..idx].trim(), Some(rest[idx + 4..].trim()))
    } else {
        (rest.trim(), None)
    };
    if id.is_empty() || id.contains(char::is_whitespace) {
        return Err(err_at(line_no, full, "некорректный идентификатор участника"));
    }
    Ok((id.to_owned(), label.filter(|l| !l.is_empty()).map(str::to_owned)))
}

/// Регистрирует участника (первое упоминание) или обновляет его подпись.
fn register_participant(
    id: &str,
    label: Option<String>,
    participants: &mut Vec<Participant>,
    ids: &mut HashMap<String, usize>,
) -> usize {
    if let Some(&i) = ids.get(id) {
        if let Some(l) = label {
            participants[i].label = l;
        }
        return i;
    }
    let i = participants.len();
    participants.push(Participant {
        id: id.to_owned(),
        label: label.unwrap_or_else(|| id.to_owned()),
    });
    ids.insert(id.to_owned(), i);
    i
}

/// Разбирает сообщение `A->>B: текст` / `A-->>B: текст`.
fn parse_message(
    text: &str,
    line_no: usize,
    participants: &mut Vec<Participant>,
    ids: &mut HashMap<String, usize>,
) -> Result<SeqMessage> {
    let mut cur = Cursor::new(text);
    let from = scan_id(&mut cur, line_no, text)?;
    cur.skip_ws();
    let dotted = if cur.eat("-->>") {
        true
    } else if cur.eat("->>") {
        false
    } else {
        return Err(err_at(line_no, text, "ожидалась стрелка '->>' или '-->>'"));
    };
    let to = scan_id(&mut cur, line_no, text)?;
    cur.skip_ws();
    if !cur.eat(":") {
        return Err(err_at(line_no, text, "ожидалось ':' и текст сообщения"));
    }
    let label = unquote(cur.rest());
    let from = register_participant(&from, None, participants, ids);
    let to = register_participant(&to, None, participants, ids);
    Ok(SeqMessage { from, to, label, dotted })
}

/// Разбирает `left of X: текст` / `right of X: текст` (после `Note `).
fn parse_note(
    rest: &str,
    line_no: usize,
    full: &str,
    participants: &mut Vec<Participant>,
    ids: &mut HashMap<String, usize>,
) -> Result<SeqNote> {
    let (side, tail) = if let Some(t) = rest.strip_prefix("left of ") {
        (NoteSide::Left, t)
    } else if let Some(t) = rest.strip_prefix("right of ") {
        (NoteSide::Right, t)
    } else {
        return Err(err_at(line_no, full, "ожидалось 'Note left of' или 'Note right of'"));
    };
    let Some(colon) = tail.find(':') else {
        return Err(err_at(line_no, full, "ожидалось ':' в Note"));
    };
    let id = tail[..colon].trim();
    let text = tail[colon + 1..].trim();
    if id.is_empty() || text.is_empty() {
        return Err(err_at(line_no, full, "пустой участник или текст Note"));
    }
    let participant = register_participant(id, None, participants, ids);
    Ok(SeqNote { participant, side, text: text.to_owned() })
}

/// Первое слово — известная неподдерживаемая конструкция sequence (пропускаем).
fn is_skippable_seq(text: &str) -> bool {
    let Some(first) = text.split_whitespace().next() else {
        return false;
    };
    matches!(
        first,
        "loop" | "alt" | "else" | "opt" | "par" | "and" | "critical" | "break" | "end"
            | "autonumber" | "activate" | "deactivate" | "create" | "destroy" | "rect" | "box"
            | "title" | "link" | "links"
    )
}

/// Разбирает sequence-диаграмму целиком.
///
/// # Ошибки
/// Некорректный заголовок, битое сообщение/заметка (с номером строки),
/// диаграмма без участников.
pub(crate) fn parse_sequence(input: &str) -> Result<SeqAst> {
    let mut header_seen = false;
    let mut participants = Vec::new();
    let mut ids = HashMap::new();
    let mut items = Vec::new();
    let mut skipped = Vec::new();
    for (idx, raw) in input.lines().enumerate() {
        let line_no = idx + 1;
        let text = strip_comment(raw).trim();
        if text.is_empty() {
            continue;
        }
        if !header_seen {
            if text == "sequenceDiagram" {
                header_seen = true;
                continue;
            }
            return Err(err_at(line_no, text, "ожидался заголовок 'sequenceDiagram'"));
        }
        if let Some(rest) = text.strip_prefix("participant ").or_else(|| text.strip_prefix("actor ")) {
            let (id, label) = parse_participant(rest, line_no, text)?;
            register_participant(&id, label, &mut participants, &mut ids);
            continue;
        }
        if text.starts_with("Note over") || is_skippable_seq(text) {
            skipped.push(Skipped { line: line_no, text: text.to_owned() });
            continue;
        }
        if let Some(rest) = text.strip_prefix("Note ") {
            let note = parse_note(rest, line_no, text, &mut participants, &mut ids)?;
            items.push(SeqItem::Note(note));
            continue;
        }
        let msg = parse_message(text, line_no, &mut participants, &mut ids)?;
        items.push(SeqItem::Message(msg));
    }
    if !header_seen {
        return Err(HarnessError::Mermaid("пустой ввод: ожидался 'sequenceDiagram'".into()));
    }
    if participants.is_empty() {
        return Err(HarnessError::Mermaid("sequenceDiagram не содержит участников".into()));
    }
    Ok(SeqAst { participants, items, skipped })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chain_and_updates_labels() {
        let ast = parse_flowchart("graph TD\nA --> B[Bee] --> C\nA[Ай]").unwrap();
        assert_eq!(ast.nodes.len(), 3);
        assert_eq!(ast.edges.len(), 2);
        assert_eq!(ast.nodes[0].label, "Ай");
        assert_eq!(ast.nodes[1].label, "Bee");
        assert_eq!(ast.nodes[2].label, "C");
    }

    #[test]
    fn parses_pipe_edge_labels() {
        // Самый частый синтаксис меток у моделей: -->|Да|.
        let ast = parse_flowchart(
            "graph TD\nA[Клиент] --> B{Авторизован?}\nB -->|Да| C[Кабинет]\nB -->|Нет| D[Логин]\nC -.->|сессия| D",
        )
        .unwrap();
        assert_eq!(ast.edges.len(), 4);
        assert_eq!(ast.edges[1].label.as_deref(), Some("Да"));
        assert_eq!(ast.edges[2].label.as_deref(), Some("Нет"));
        assert_eq!(ast.edges[2].to, 3, "D — четвёртый узел");
        assert_eq!(ast.nodes[1].shape, Shape::Rhombus);
    }

    #[test]
    fn pipe_label_needs_closing_bar() {
        let err = parse_flowchart("graph TD\nA -->|нет закрывающей B\n").unwrap_err();
        assert!(err.to_string().contains("pipe-метка"), "{}", err);
    }

    #[test]
    fn parses_all_shapes_and_edge_kinds() {
        let ast = parse_flowchart(
            "flowchart LR\nA[rect] --> B(rounded)\nB -.-> C{rhombus}\nC --- D((circle))\nA -- метка --> D",
        )
        .unwrap();
        assert_eq!(ast.nodes.len(), 4);
        assert_eq!(ast.edges.len(), 4);
        assert_eq!(ast.nodes[2].shape, Shape::Rhombus);
        assert_eq!(ast.nodes[3].shape, Shape::Circle);
        assert!(ast.edges[2].plain);
        assert_eq!(ast.edges[3].label.as_deref(), Some("метка"));
    }

    #[test]
    fn quoted_label_protects_arrow_inside() {
        let ast = parse_flowchart("graph LR\nA[\"текст с --> внутри\"] --> B").unwrap();
        assert_eq!(ast.nodes[0].label, "текст с --> внутри");
        assert_eq!(ast.edges.len(), 1);
    }

    #[test]
    fn skips_unknown_constructs_with_warning() {
        let ast = parse_flowchart(
            "flowchart LR\nsubgraph x\nA --> B\nend\nclick A cb\nstyle A fill:#f9f",
        )
        .unwrap();
        assert_eq!(ast.skipped.len(), 4);
        assert_eq!(ast.edges.len(), 1);
    }

    #[test]
    fn errors_have_line_numbers() {
        let err = parse_flowchart("graph TD\nA --> B\nC -->\n").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("строка 3"), "нет номера строки: {msg}");
    }

    #[test]
    fn parses_sequence_parts() {
        let ast = parse_sequence(
            "sequenceDiagram\nparticipant A as Alpha\nparticipant B\nA->>B: hi\nNote left of B: hmm\nB-->>A: ok",
        )
        .unwrap();
        assert_eq!(ast.participants.len(), 2);
        assert_eq!(ast.participants[0].label, "Alpha");
        assert_eq!(ast.items.len(), 3);
        assert!(matches!(&ast.items[2], SeqItem::Message(m) if m.dotted));
    }
}
