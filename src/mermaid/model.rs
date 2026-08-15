//! Внутренние структуры AST диаграмм (flowchart и sequence).

/// Направление flowchart-диаграммы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    /// Сверху вниз (`TD`/`TB`).
    TopDown,
    /// Снизу вверх (`BT`).
    BottomUp,
    /// Слева направо (`LR`).
    LeftRight,
    /// Справа налево (`RL`).
    RightLeft,
}

impl Direction {
    /// Горизонтальное ли направление (`LR`/`RL`).
    pub(crate) fn is_horizontal(self) -> bool {
        matches!(self, Self::LeftRight | Self::RightLeft)
    }
}

/// Форма узла flowchart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    /// `A[label]` — прямоугольник.
    Rect,
    /// `A(label)` — скруглённый прямоугольник.
    Rounded,
    /// `A{label}` — ромб (упрощённое обрамление `< … >`, `╱ ╲`).
    Rhombus,
    /// `A((label))` — круг (обрамление `(( … ))`).
    Circle,
}

/// Узел flowchart.
#[derive(Debug, Clone)]
pub(crate) struct FlowNode {
    /// Идентификатор узла.
    #[allow(dead_code)] // читается в тестах раскладки и нужен для диагностики графа
    pub id: String,
    /// Подпись (по умолчанию равна идентификатору).
    pub label: String,
    /// Форма рамки.
    pub shape: Shape,
}

/// Ребро flowchart (узлы — индексы в `FlowAst::nodes`).
#[derive(Debug, Clone)]
pub(crate) struct FlowEdge {
    /// Узел-источник.
    pub from: usize,
    /// Узел-назначение.
    pub to: usize,
    /// Метка ребра (`-- метка -->`).
    pub label: Option<String>,
    /// `---` — линия без стрелки.
    pub plain: bool,
}

/// Предупреждение о пропущенной (неподдерживаемой) конструкции.
#[derive(Debug, Clone)]
pub(crate) struct Skipped {
    /// Номер строки во входе (с 1).
    pub line: usize,
    /// Текст строки.
    pub text: String,
}

/// AST flowchart-диаграммы.
#[derive(Debug)]
pub(crate) struct FlowAst {
    /// Направление.
    pub dir: Direction,
    /// Узлы в порядке первого упоминания.
    pub nodes: Vec<FlowNode>,
    /// Рёбра в порядке объявления.
    pub edges: Vec<FlowEdge>,
    /// Пропущенные конструкции.
    pub skipped: Vec<Skipped>,
}

/// Участник sequence-диаграммы.
#[derive(Debug, Clone)]
pub(crate) struct Participant {
    /// Идентификатор (`participant X as Label` → `X`).
    #[allow(dead_code)] // идентификатор хранится для отладки/будущих проверок связей
    pub id: String,
    /// Отображаемая подпись (по умолчанию = идентификатор).
    pub label: String,
}

/// Сообщение между участниками.
#[derive(Debug, Clone)]
pub(crate) struct SeqMessage {
    /// Отправитель (индекс в `SeqAst::participants`).
    pub from: usize,
    /// Получатель (индекс; равен `from` при самовызове).
    pub to: usize,
    /// Текст сообщения (после `:`).
    pub label: String,
    /// `-->>` — пунктир (рисуется `┄`).
    pub dotted: bool,
}

/// Сторона заметки относительно линии жизни.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoteSide {
    /// `Note left of X`.
    Left,
    /// `Note right of X`.
    Right,
}

/// Заметка рядом с линией жизни участника.
#[derive(Debug, Clone)]
pub(crate) struct SeqNote {
    /// Участник (индекс).
    pub participant: usize,
    /// Сторона.
    pub side: NoteSide,
    /// Текст заметки.
    pub text: String,
}

/// Элемент sequence-диаграммы в порядке объявления.
#[derive(Debug, Clone)]
pub(crate) enum SeqItem {
    /// Сообщение.
    Message(SeqMessage),
    /// Заметка.
    Note(SeqNote),
}

/// AST sequence-диаграммы.
#[derive(Debug)]
pub(crate) struct SeqAst {
    /// Участники в порядке объявления/первого упоминания.
    pub participants: Vec<Participant>,
    /// Сообщения и заметки сверху вниз.
    pub items: Vec<SeqItem>,
    /// Пропущенные конструкции.
    pub skipped: Vec<Skipped>,
}
