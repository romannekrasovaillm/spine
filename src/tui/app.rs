//! Состояние TUI-приложения: блоки чата, строка ввода, вкладки, обработка
//! клавиш и сообщений фоновых задач. App владеет состоянием в event loop —
//! никаких `Arc<Mutex>`; ход агента и слэш-команды выполняются в `tokio::spawn`
//! и возвращают сессию сообщением.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use tokio::sync::mpsc;

use crate::agent::{AgentEvent, AgentSession, prompts, slash};
use crate::config::Config;
use crate::error::Result;
use crate::llm::{ChatMessage, LlmRegistry};
use crate::mcp::{self, McpManager};
use crate::tool::{AskRequest, ToolContext};
use crate::tools;

use super::text;
use super::theme::Theme;

/// Максимум блоков в истории чата (сверху отбрасываются самые старые).
const MAX_BLOCKS: usize = 500;
/// Ёмкость канала событий агента (bounded — backpressure до модели).
const AGENT_EVENTS_CAP: usize = 64;
/// Максимум записей в истории ввода.
const MAX_HISTORY: usize = 100;
/// Максимум сообщений в очереди ожидания (пока агент занят).
const MAX_QUEUE: usize = 32;
/// Кадры спиннера ожидания модели (брайлевская анимация).
pub(crate) const SPINNER: [&str; 10] = ["⠋", "⠙", "⠚", "⠞", "⠖", "⠦", "⠴", "⠲", "⠳", "⠓"];

/// Встроенный системный промпт (fallback, если шаблона `architect` нет).
const FALLBACK_SYSTEM_PROMPT: &str = "Ты — solution-архитектор в корпоративном контуре банка. \
     Помогаешь проектировать решения, ведёшь ADR и architecture-spine, оцениваешь архитектуру \
     по рубрикам, готовишь handoff-пакеты кодовым агентам. Отвечай по-русски, точно и по делу.";

/// Активный экран приложения.
#[derive(Debug)]
pub(crate) enum Screen {
    /// Стартовый экран с баннером (любая клавиша → чат).
    Splash,
    /// Основной чат.
    Chat,
    /// Фатальная ошибка инициализации: показать текст и выйти по `q`.
    Fatal(String),
}

/// Состояние вызова инструмента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolState {
    /// Выполняется.
    Running,
    /// Завершился успешно.
    Ok,
    /// Завершился ошибкой.
    Error,
}

/// Блок чата (центральная колонка).
#[derive(Debug)]
pub(crate) enum ChatBlock {
    /// Сообщение пользователя.
    User(String),
    /// Ответ ассистента (стримится дельтами).
    Assistant(String),
    /// Вызов инструмента.
    Tool {
        /// Имя инструмента.
        name: String,
        /// Состояние выполнения.
        state: ToolState,
        /// Краткий итог (первая строка вывода).
        summary: String,
    },
    /// Результат слэш-команды.
    System {
        /// Введённая команда.
        command: String,
        /// Текст результата.
        text: String,
    },
    /// Ошибка (красный блок).
    Error(String),
}

/// Вкладка правой панели.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RightTab {
    /// Последний рендер mermaid-диаграммы.
    Mermaid,
    /// Последний отчёт рубрики.
    Rubric,
    /// Последний результат поиска по знаниям/вебу.
    Knowledge,
}

impl RightTab {
    /// Все вкладки по порядку.
    pub(crate) const ALL: [Self; 3] = [Self::Mermaid, Self::Rubric, Self::Knowledge];

    /// Следующая вкладка (цикл по Tab).
    fn next(self) -> Self {
        match self {
            Self::Mermaid => Self::Rubric,
            Self::Rubric => Self::Knowledge,
            Self::Knowledge => Self::Mermaid,
        }
    }

    /// Заголовок вкладки.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Mermaid => "Mermaid",
            Self::Rubric => "Рубрика",
            Self::Knowledge => "Знания",
        }
    }
}

/// Содержимое вкладок правой панели.
#[derive(Debug, Default)]
pub(crate) struct Panels {
    /// Вкладка Mermaid: последний рендер диаграммы.
    pub(crate) mermaid: String,
    /// Вкладка «Рубрика»: последний отчёт.
    pub(crate) rubric: String,
    /// Вкладка «Знания»: последний результат kb/web.
    pub(crate) knowledge: String,
}

impl Panels {
    /// Текст активной вкладки.
    pub(crate) fn content(&self, tab: RightTab) -> &str {
        match tab {
            RightTab::Mermaid => &self.mermaid,
            RightTab::Rubric => &self.rubric,
            RightTab::Knowledge => &self.knowledge,
        }
    }

    /// Подсказка-заглушка пустой вкладки.
    pub(crate) fn placeholder(tab: RightTab) -> &'static str {
        match tab {
            RightTab::Mermaid => {
                "Пока пусто. Здесь появится последний рендер диаграммы \
                 (инструмент mermaid_render или /mermaid)."
            }
            RightTab::Rubric => "Пока пусто. Здесь появится последний отчёт рубрики (/rubric run …).",
            RightTab::Knowledge => {
                "Пока пусто. Здесь появится последний результат поиска (/kb, /web, /fetch)."
            }
        }
    }
}

/// Сообщение в event loop от фоновых задач.
pub(crate) enum AppMessage {
    /// Событие агентного цикла (дельта, инструмент, конец хода).
    AgentEvent(AgentEvent),
    /// Инструмент `propose_options` просит пользователя выбрать вариант.
    AskUser(AskRequest),
    /// Ход агента завершён: сессия возвращается владельцу (App).
    TurnFinished {
        /// Сессия после хода.
        session: AgentSession,
        /// Итог хода (финальный текст или ошибка).
        result: Result<String>,
    },
    /// Слэш-команда завершена.
    SlashFinished {
        /// Сессия после команды.
        session: AgentSession,
        /// Исход команды.
        result: Result<slash::SlashOutcome>,
    },
}

/// Состояние строки ввода: текст, курсор, история, автодополнение.
#[derive(Debug, Default)]
pub(crate) struct InputState {
    /// Текст ввода.
    text: String,
    /// Позиция курсора (байтовый индекс, всегда на границе char).
    cursor: usize,
    /// История отправленных строк (старые — в начале).
    history: VecDeque<String>,
    /// Индекс навигации по истории (None — редактируется черновик).
    hist_idx: Option<usize>,
    /// Черновик, сохранённый при уходе в историю.
    draft: String,
    /// Активные кандидаты автодополнения и текущий индекс (цикл по Tab).
    completion: Option<(Vec<&'static str>, usize)>,
}

impl InputState {
    /// Текущий текст.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Позиция курсора (байтовый индекс).
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    /// Устанавливает текст, курсор — в конец; сбрасывает автодополнение.
    pub(crate) fn set_text(&mut self, text: String) {
        self.text = text;
        self.cursor = self.text.len();
        self.completion = None;
    }

    /// Вставляет символ в позицию курсора.
    fn insert_char(&mut self, c: char) {
        self.completion = None;
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Удаляет символ перед курсором.
    fn backspace(&mut self) {
        self.completion = None;
        if self.cursor == 0 {
            return;
        }
        let prev = self.text[..self.cursor]
            .chars()
            .next_back()
            .map_or(1, char::len_utf8);
        self.text.replace_range(self.cursor - prev..self.cursor, "");
        self.cursor -= prev;
    }

    /// Удаляет символ под курсором.
    fn delete(&mut self) {
        self.completion = None;
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.text
                .replace_range(self.cursor..self.cursor + c.len_utf8(), "");
        }
    }

    /// Курсор на символ влево.
    fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.text[..self.cursor]
                .chars()
                .next_back()
                .map_or(0, |c| self.cursor - c.len_utf8());
        }
    }

    /// Курсор на символ вправо.
    fn move_right(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    /// Курсор в начало строки.
    fn move_home(&mut self) {
        self.cursor = 0;
    }

    /// Курсор в конец строки.
    fn move_end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Забирает введённую строку, сохраняя непустую в истории.
    fn submit(&mut self) -> String {
        let text = self.text.trim().to_string();
        if !text.is_empty() && self.history.back() != Some(&text) {
            self.history.push_back(text.clone());
            while self.history.len() > MAX_HISTORY {
                self.history.pop_front();
            }
        }
        self.text.clear();
        self.cursor = 0;
        self.hist_idx = None;
        self.draft.clear();
        self.completion = None;
        text
    }

    /// Up: шаг назад по истории (черновик сохраняется).
    fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let idx = match self.hist_idx {
            None => {
                self.draft.clone_from(&self.text);
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.hist_idx = Some(idx);
        if let Some(entry) = self.history.get(idx) {
            self.set_text(entry.clone());
        }
    }

    /// Down: шаг вперёд по истории; за последней записью — черновик.
    fn history_down(&mut self) {
        let Some(idx) = self.hist_idx else {
            return;
        };
        if idx + 1 < self.history.len() {
            self.hist_idx = Some(idx + 1);
            if let Some(entry) = self.history.get(idx + 1) {
                self.set_text(entry.clone());
            }
        } else {
            self.hist_idx = None;
            let draft = std::mem::take(&mut self.draft);
            self.set_text(draft);
        }
    }

    /// Tab: дополняет слэш-команду; повторные Tab циклят кандидатов.
    /// Возвращает false, если кандидатов нет (Tab свободен для вкладок).
    fn complete_tab(&mut self) -> bool {
        // Уже в цикле дополнения и текст не редактировали — следующий кандидат.
        if let Some((cands, idx)) = &mut self.completion {
            if self.text == cands[*idx] {
                *idx = (*idx + 1) % cands.len();
                let candidate = cands[*idx];
                // Напрямую, не через set_text: цикл кандидатов сохраняем.
                self.text = candidate.to_string();
                self.cursor = self.text.len();
                return true;
            }
        }
        self.completion = None;
        let cands = text::completion_candidates(&self.text);
        if cands.is_empty() {
            return false;
        }
        self.set_text(cands[0].to_string());
        self.completion = Some((cands, 0));
        true
    }

    /// Приглушённая подсказка-дополнение справа от ввода (суффикс кандидата).
    pub(crate) fn ghost_hint(&self) -> Option<String> {
        let first = text::completion_candidates(&self.text).into_iter().next()?;
        Some(first[self.text.len()..].to_string())
    }
}

/// Приложение TUI: владеет всем состоянием, события приходят по каналу.
/// Назначение активной модальной панели выбора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AskKind {
    /// Ответ уходит инструменту `propose_options` (oneshot-канал, агент ждёт).
    Tool,
    /// Пикер модели (`/model` без аргумента): выбор сводится к `/model <name>`.
    ModelPicker,
}

/// Активная модальная панель выбора (инструмент `propose_options` ждёт ответа).
#[derive(Debug)]
pub(crate) struct AskState {
    /// Вопрос агента.
    pub(crate) question: String,
    /// Варианты (2–4; у пикера моделей — по числу `[models]`).
    pub(crate) options: Vec<crate::tool::AskOption>,
    /// `label` рекомендуемого варианта (если есть).
    pub(crate) recommended: Option<String>,
    /// Индекс подсвеченного варианта.
    pub(crate) selected: usize,
    /// Канал ответа инструменту (None — пикер или ответ уже отправлен).
    pub(crate) reply: Option<tokio::sync::oneshot::Sender<String>>,
    /// Назначение модалки.
    pub(crate) kind: AskKind,
}

/// Полноэкранный просмотрщик активной вкладки (F4): вертикальный и
/// ГОРИЗОНТАЛЬНЫЙ скролл широкого mermaid-арта. Основной экран не
/// перестраивается — viewer перекрывает его модальным слоем.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ViewerState {
    /// Вертикальный сдвиг (строки сверху).
    pub(crate) scroll_y: usize,
    /// Горизонтальный сдвиг (display-колонки слева).
    pub(crate) scroll_x: usize,
}

pub(crate) struct App {
    /// Активный экран.
    pub(crate) screen: Screen,
    /// Активный вопрос выбора вариантов (модалка поверх чата).
    pub(crate) ask: Option<AskState>,
    /// Полноэкранный просмотрщик вкладки (F4); None — обычный лейаут.
    pub(crate) viewer: Option<ViewerState>,
    /// Приёмник запросов выбора от инструментов (форвардится в attach).
    ask_rx: Option<mpsc::Receiver<AskRequest>>,
    /// Сессия агента (None — пока ход выполняется в фоновой задаче).
    session: Option<AgentSession>,
    /// Контекст инструментов (для слэш-команд; cwd — в статус-баре).
    pub(crate) tool_ctx: ToolContext,
    /// MCP-менеджер (нужен для shutdown); None, если не подключён.
    mcp: Option<Arc<McpManager>>,
    /// Блоки чата.
    pub(crate) blocks: Vec<ChatBlock>,
    /// Открыт ли сейчас assistant-блок для дельт.
    assistant_open: bool,
    /// Была ли хотя бы одна дельта за текущий ход.
    turn_got_output: bool,
    /// Строка ввода.
    pub(crate) input: InputState,
    /// Сдвиг прокрутки чата от низа (0 — прилип к низу).
    pub(crate) scroll: usize,
    /// Авто-прилипание к низу при новых событиях.
    stick: bool,
    /// Высота вьюпорта чата (заполняется при рендере).
    pub(crate) viewport: usize,
    /// Активная вкладка правой панели.
    pub(crate) right_tab: RightTab,
    /// Содержимое вкладок правой панели.
    pub(crate) panels: Panels,
    /// Доп. сообщение в статус-баре (например, ошибка MCP).
    status_extra: Option<String>,
    /// Идёт фоновый ход (модель/команда) — ввод складывается в очередь.
    thinking: bool,
    /// Очередь сообщений, набранных во время хода агента (FIFO;
    /// срочные — в начало через Alt+Enter или префикс «!!»).
    pub(crate) queue: VecDeque<String>,
    /// Кадр спиннера.
    spinner: usize,
    /// Команда, ожидающая результата (для привязки вывода к вкладкам).
    pending_slash: Option<String>,
    /// Флаг выхода из event loop.
    pub(crate) should_quit: bool,
    /// Имя модели для статус-бара.
    pub(crate) model_name: String,
    /// Грубая оценка токенов истории.
    history_tokens: usize,
    /// Эффективный бюджет контекста активной модели (0 — неизвестен).
    context_budget: usize,
    /// Область кнопки «▼ — к свежему ответу» (ставит рендер; None — у дна).
    pub(crate) jump_btn: Option<ratatui::layout::Rect>,
    /// Тема оформления.
    pub(crate) theme: Theme,
    /// Канал событий в event loop.
    msg_tx: Option<mpsc::Sender<AppMessage>>,
}

impl App {
    /// Собирает приложение: LLM-реестр, инструменты, MCP (мягко), сессию.
    /// Ошибка инициализации модели → экран [`Screen::Fatal`] (выход по `q`).
    pub(crate) async fn build(cfg: Arc<Config>) -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let registry = match LlmRegistry::from_config(&cfg) {
            Ok(r) => Arc::new(r),
            Err(e) => {
                let tool_ctx = ToolContext::new(cwd, cfg);
                return Self::fatal(tool_ctx, format!("{e}"));
            }
        };
        let provider = registry.default();
        let model_name = format!("{}:{}", registry.default_name(), provider.model());

        let mut tools = tools::full_registry(&cfg);
        let mut status_extra = None;
        let mut mcp_manager = None;
        // MCP по умолчанию ленивый (connect_on_start=false): старт TUI не ждёт
        // npx/uvx-загрузок серверов. Инструменты MCP доступны модели только при
        // connect_on_start=true; слэш `/mcp` работает в любом режиме.
        if cfg.mcp.connect_on_start && cfg.mcp.servers_file.is_file() {
            match mcp::load_servers(&cfg.mcp.servers_file) {
                Ok(servers) if !servers.is_empty() => {
                    match McpManager::connect(&servers, cfg.mcp.timeout_secs).await {
                        Ok(manager) => {
                            let manager = Arc::new(manager);
                            mcp::register_mcp_tools(&mut tools, &manager).await;
                            mcp_manager = Some(manager);
                        }
                        Err(e) => status_extra = Some(format!("MCP недоступен: {e}")),
                    }
                }
                Ok(_) => {}
                Err(e) => status_extra = Some(format!("MCP: {e}")),
            }
        } else if cfg.mcp.servers_file.is_file() {
            status_extra = Some("MCP: ленивый режим (/mcp list — подключить)".into());
        }

        let tool_ctx = ToolContext::new(cwd, cfg.clone())
            .with_llm(registry)
            .with_provider(provider.clone())
            .with_subagents(crate::subagent::SubagentRegistry::new());
        // Мост интерактивных вопросов: инструмент propose_options → модалка.
        let (ask_tx, ask_rx) = mpsc::channel::<AskRequest>(8);
        let tool_ctx = tool_ctx.with_ask(ask_tx);
        let system = system_prompt(&cfg);
        let session = AgentSession::new(cfg, provider, tools, tool_ctx.clone(), system);
        let mut app = Self::new(Some(session), tool_ctx, mcp_manager, status_extra);
        app.ask_rx = Some(ask_rx);
        app.model_name = model_name;
        app
    }

    /// Приложение в состоянии фатальной ошибки (показать экран и выйти).
    fn fatal(tool_ctx: ToolContext, error: String) -> Self {
        let mut app = Self::new(None, tool_ctx, None, None);
        app.screen = Screen::Fatal(error);
        app
    }

    /// Конструктор состояния по умолчанию (чат с экрана-заставки).
    fn new(
        session: Option<AgentSession>,
        tool_ctx: ToolContext,
        mcp: Option<Arc<McpManager>>,
        status_extra: Option<String>,
    ) -> Self {
        let context_budget = session
            .as_ref()
            .map_or(0, AgentSession::effective_context_budget);
        Self {
            screen: Screen::Splash,
            ask: None,
            viewer: None,
            ask_rx: None,
            session,
            tool_ctx,
            mcp,
            blocks: Vec::new(),
            assistant_open: false,
            turn_got_output: false,
            input: InputState::default(),
            scroll: 0,
            stick: true,
            viewport: 1,
            right_tab: RightTab::Mermaid,
            panels: Panels::default(),
            status_extra,
            thinking: false,
            queue: VecDeque::new(),
            spinner: 0,
            pending_slash: None,
            should_quit: false,
            model_name: "—".into(),
            history_tokens: 0,
            context_budget,
            jump_btn: None,
            theme: Theme::default(),
            msg_tx: None,
        }
    }

    /// Подключает канал сообщений event loop и форвардер вопросов выбора
    /// (`propose_options` → модалка). Вызывается один раз из `tui::run`.
    pub(crate) fn attach(&mut self, tx: mpsc::Sender<AppMessage>) {
        self.msg_tx = Some(tx.clone());
        if let Some(mut rx) = self.ask_rx.take() {
            tokio::spawn(async move {
                while let Some(req) = rx.recv().await {
                    if tx.send(AppMessage::AskUser(req)).await.is_err() {
                        break;
                    }
                }
            });
        }
    }

    /// Отрисовка текущего экрана.
    pub(crate) fn render(&mut self, f: &mut Frame<'_>) {
        super::render::draw(f, self);
    }

    /// Нужны ли периодические тики (спиннер ожидания).
    /// Тики нужны, пока идёт ход ИЛИ работают фоновые субагенты
    /// (индикатор в статус-баре должен крутиться и обновляться).
    pub(crate) fn needs_tick(&self) -> bool {
        self.thinking || self.subagents_running() > 0
    }

    /// Число работающих фоновых задач (субагенты и ralph-циклы делят слоты).
    pub(crate) fn subagents_running(&self) -> usize {
        self.tool_ctx
            .subagents
            .as_ref()
            .map(|r| r.running())
            .unwrap_or(0)
    }

    /// Идёт ли фоновый ход (модель/команда).
    pub(crate) fn thinking(&self) -> bool {
        self.thinking
    }

    /// Текущий кадр спиннера ожидания.
    pub(crate) fn spinner_frame(&self) -> usize {
        self.spinner
    }

    /// Активная вкладка правой панели.
    pub(crate) fn right_tab(&self) -> RightTab {
        self.right_tab
    }

    /// Грубая оценка токенов истории.
    pub(crate) fn history_tokens(&self) -> usize {
        self.history_tokens
    }

    /// Эффективный бюджет контекста активной модели (0 — неизвестен).
    pub(crate) fn context_budget(&self) -> usize {
        self.context_budget
    }

    /// Доп. сообщение статус-бара (ошибки MCP и т.п.).
    pub(crate) fn status_extra(&self) -> Option<&str> {
        self.status_extra.as_deref()
    }

    /// Тик таймера: кадр спиннера.
    pub(crate) fn tick(&mut self) {
        self.spinner = (self.spinner + 1) % SPINNER.len();
    }

    /// Graceful shutdown фоновых ресурсов (MCP-серверы).
    pub(crate) async fn shutdown(&self) {
        if let Some(manager) = &self.mcp {
            manager.shutdown().await;
        }
    }

    /// Обработка клавиши.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) {
        // Ctrl-C — выход всегда.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c'))
        {
            self.should_quit = true;
            return;
        }
        // Модалка выбора (propose_options) перехватывает клавиши: агент
        // заблокирован в ожидании ответа, обычный ввод недоступен.
        if matches!(self.screen, Screen::Chat) && self.ask.is_some() {
            self.handle_ask_key(key);
            return;
        }
        // Просмотрщик вкладки (F4) перехватывает клавиши: навигация по арту,
        // Esc здесь — «назад», а не выход из приложения.
        if matches!(self.screen, Screen::Chat) && self.viewer.is_some() {
            self.handle_viewer_key(key);
            return;
        }
        match &self.screen {
            Screen::Splash => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
                _ => self.screen = Screen::Chat,
            },
            Screen::Fatal(_) => match key.code {
                KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => {
                    self.should_quit = true;
                }
                _ => {}
            },
            Screen::Chat => self.handle_chat_key(key),
        }
    }

    /// Событие мыши: колесо прокручивает диалог (3 строки за тик);
    /// в просмотрщике — его вертикаль, а с Shift — горизонтальная панорама.
    /// Клик левой кнопкой по «▼» в правом нижнем углу диалога — прыжок
    /// к свежему ответу. Работает и во время хода модели (как PgUp/PgDn).
    pub(crate) fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        if !matches!(self.screen, Screen::Chat) {
            return;
        }
        const WHEEL_LINES: usize = 3;
        use crossterm::event::MouseEventKind as K;
        if let Some(mut v) = self.viewer {
            let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
            match (mouse.kind, shift) {
                (K::ScrollUp, true) => v.scroll_x = v.scroll_x.saturating_sub(8),
                (K::ScrollDown, true) => v.scroll_x = v.scroll_x.saturating_add(8),
                (K::ScrollUp, false) => v.scroll_y = v.scroll_y.saturating_sub(WHEEL_LINES),
                (K::ScrollDown, false) => v.scroll_y = v.scroll_y.saturating_add(WHEEL_LINES),
                _ => {}
            }
            self.viewer = Some(v);
            return;
        }
        match mouse.kind {
            // Клик по кнопке «▼» (справа внизу диалога) — к свежему ответу.
            K::Down(crossterm::event::MouseButton::Left)
                if self.jump_btn.is_some_and(|r| {
                    r.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                }) =>
            {
                self.scroll_to_bottom();
            }
            K::ScrollUp => self.scroll_by(WHEEL_LINES),
            K::ScrollDown => self.scroll_back(WHEEL_LINES),
            _ => {}
        }
    }

    fn handle_chat_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.should_quit = true;
            return;
        }        match key.code {
            // Прокрутка и вкладки доступны и во время хода модели.
            KeyCode::PageUp => self.scroll_by(self.page()),
            KeyCode::PageDown => self.scroll_back(self.page()),
            KeyCode::F(1) => self.right_tab = RightTab::Mermaid,
            KeyCode::F(2) => self.right_tab = RightTab::Rubric,
            KeyCode::F(3) => self.right_tab = RightTab::Knowledge,
            KeyCode::F(4) => self.toggle_viewer(),
            // Во время хода ввод НЕ блокируется: Enter — сообщение в очередь
            // (FIFO), Alt+Enter — срочно в начало (выполнится следующим).
            KeyCode::Enter
                if self.thinking && key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.enqueue_typed(true);
            }
            KeyCode::Enter if self.thinking => self.enqueue_typed(false),
            KeyCode::Enter => self.submit(),
            KeyCode::Tab => {
                if !self.input.complete_tab() {
                    self.right_tab = self.right_tab.next();
                }
            }
            KeyCode::Up => self.input.history_up(),
            KeyCode::Down => self.input.history_down(),
            KeyCode::Left => self.input.move_left(),
            KeyCode::Right => self.input.move_right(),
            KeyCode::Home => self.input.move_home(),
            KeyCode::End => self.input.move_end(),
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Char('q') if self.input.text.is_empty() && key.modifiers.is_empty() => {
                self.should_quit = true;
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.input.insert_char(c);
            }
            _ => {}
        }
    }

    /// Enter во время хода: набранный текст — в очередь (`front` — в начало,
    /// срочное). Префикс «!!» — срочно даже без Alt (терминали без Alt+Enter).
    fn enqueue_typed(&mut self, front: bool) {
        let input = self.input.submit();
        if input.is_empty() {
            return;
        }
        let (front, input) = match input.strip_prefix("!!") {
            Some(rest) => (true, rest.trim_start().to_string()),
            None => (front, input),
        };
        if input.is_empty() {
            return;
        }
        if self.queue.len() >= MAX_QUEUE {
            self.push_block(ChatBlock::Error(format!(
                "очередь полна ({MAX_QUEUE}) — дождитесь завершения хода"
            )));
            return;
        }
        if front {
            self.queue.push_front(input);
        } else {
            self.queue.push_back(input);
        }
    }

    /// Открывает модалку выбора по запросу инструмента `propose_options`.
    /// Если предыдущий вопрос ещё висит (нештатно), он отклоняется — инструмент
    /// не должен ждать вечно.
    fn open_ask(&mut self, req: AskRequest) {
        if let Some(prev) = self.ask.take() {
            answer(prev.reply, String::new());
        }
        let selected = req
            .recommended
            .as_deref()
            .and_then(|rec| req.options.iter().position(|o| o.label == rec))
            .unwrap_or(0);
        self.ask = Some(AskState {
            question: req.question,
            options: req.options,
            recommended: req.recommended,
            selected,
            reply: Some(req.reply),
            kind: AskKind::Tool,
        });
        self.scroll_to_bottom();
    }

    /// Открывает пикер моделей по `/model` без аргумента: варианты — ключи
    /// `[models]` из конфига (описание — id модели и доступность `/think`),
    /// ★ и курсор — текущая модель. Выбор сводится к обычному `/model <name>`.
    pub(crate) fn open_model_picker(&mut self) {
        let current = self
            .session
            .as_ref()
            .map(|s| s.provider().name().to_string())
            .unwrap_or_default();
        let options: Vec<crate::tool::AskOption> = self
            .tool_ctx
            .config
            .models
            .iter()
            .map(|(name, mc)| {
                let think = if mc.thinking_on.is_some() {
                    " · ризонинг: /think"
                } else {
                    ""
                };
                crate::tool::AskOption {
                    label: name.clone(),
                    description: format!("{}{think}", mc.model),
                }
            })
            .collect();
        if options.is_empty() {
            return;
        }
        let selected = options
            .iter()
            .position(|o| o.label == current)
            .unwrap_or(0);
        self.ask = Some(AskState {
            question: format!("Модель для этой сессии (сейчас: {current}):"),
            options,
            recommended: (!current.is_empty()).then_some(current),
            selected,
            reply: None,
            kind: AskKind::ModelPicker,
        });
        self.scroll_to_bottom();
    }

    /// Клавиши модалки выбора: навигация, подтверждение, отказ.
    fn handle_ask_key(&mut self, key: KeyEvent) {
        let Some(ask) = self.ask.as_mut() else {
            return;
        };
        let count = ask.options.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                ask.selected = ask.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                ask.selected = (ask.selected + 1).min(count.saturating_sub(1));
            }
            KeyCode::Home => ask.selected = 0,
            KeyCode::End => ask.selected = count.saturating_sub(1),
            KeyCode::Enter => {
                let label = ask.options.get(ask.selected).map(|o| o.label.clone());
                self.answer_ask(label);
            }
            KeyCode::Char(c) if ('1'..='9').contains(&c) => {
                let idx = (c.to_digit(10).unwrap_or(1) - 1) as usize;
                let label = if idx < count {
                    Some(ask.options[idx].label.clone())
                } else {
                    None
                };
                if label.is_some() {
                    self.answer_ask(label);
                }
            }
            // Отказ — пустой ответ: инструмент превратит его в «реши сам».
            KeyCode::Esc => self.answer_ask(None),
            _ => {}
        }
    }

    /// Отправляет ответ и закрывает модалку. `None` — отказ (Esc): инструменту
    /// уходит пустой ответ («реши сам»), пикер просто закрывается.
    fn answer_ask(&mut self, label: Option<String>) {
        if let Some(ask) = self.ask.take() {
            match ask.kind {
                AskKind::Tool => answer(ask.reply, label.unwrap_or_default()),
                AskKind::ModelPicker => {
                    if let Some(label) = label {
                        self.start_slash(format!("/model {label}"));
                    }
                }
            }
        }
        // Модалка могла задержать очередь — запускаем, если свободны.
        self.maybe_start_queued();
    }

    /// F4: открыть/закрыть полноэкранный просмотр активной вкладки.
    /// При открытии скролл сбрасывается (начинаем с верхнего левого угла).
    fn toggle_viewer(&mut self) {
        self.viewer = if self.viewer.is_some() {
            None
        } else {
            Some(ViewerState::default())
        };
    }

    /// Клавиши просмотрщика вкладки: прокрутка (в т.ч. горизонтальная
    /// панорама широкого арта), смена вкладки, закрытие.
    fn handle_viewer_key(&mut self, key: KeyEvent) {
        let Some(mut v) = self.viewer else {
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(4) => {
                self.viewer = None;
                return;
            }
            KeyCode::Up => v.scroll_y = v.scroll_y.saturating_sub(1),
            KeyCode::Down => v.scroll_y = v.scroll_y.saturating_add(1),
            KeyCode::Left => v.scroll_x = v.scroll_x.saturating_sub(8),
            KeyCode::Right => v.scroll_x = v.scroll_x.saturating_add(8),
            KeyCode::PageUp => v.scroll_y = v.scroll_y.saturating_sub(12),
            KeyCode::PageDown => v.scroll_y = v.scroll_y.saturating_add(12),
            KeyCode::Home => {
                v.scroll_x = 0;
                v.scroll_y = 0;
            }
            // Вкладки переключаются и в просмотрщике (скролл — новой вкладки).
            KeyCode::F(1) => {
                self.right_tab = RightTab::Mermaid;
                v = ViewerState::default();
            }
            KeyCode::F(2) => {
                self.right_tab = RightTab::Rubric;
                v = ViewerState::default();
            }
            KeyCode::F(3) => {
                self.right_tab = RightTab::Knowledge;
                v = ViewerState::default();
            }
            _ => {}
        }
        self.viewer = Some(v);
    }

    /// Enter: отправка ввода — слэш-команде или модели; во время хода —
    /// постановка в очередь (см. [`App::enqueue_typed`]).
    fn submit(&mut self) {
        let input = self.input.submit();
        if input.is_empty() {
            return;
        }
        if self.thinking {
            // Гонка отрисовки: ход уже начался — в конец очереди.
            if self.queue.len() < MAX_QUEUE {
                self.queue.push_back(input);
            }
            return;
        }
        self.submit_text(input);
    }

    /// Немедленный запуск сообщения: блок пользователя + ход/слэш/export.
    fn submit_text(&mut self, input: String) {
        self.scroll_to_bottom();
        self.push_block(ChatBlock::User(input.clone()));
        // `/export` перехватывается здесь: блоки диалога видны только TUI,
        // слэш-слой работает с сессией и про экран не знает.
        if input.trim_start().starts_with("/export") {
            self.do_export(&input);
        } else if slash::is_slash(&input) {
            self.start_slash(input);
        } else {
            self.start_turn(input);
        }
    }

    /// Запуск первого сообщения из очереди (после завершения хода/команды).
    /// Ждёт, если открыта модалка выбора (ответ пользователя важнее).
    fn maybe_start_queued(&mut self) {
        if self.thinking || self.ask.is_some() || self.viewer.is_some() {
            return;
        }
        if let Some(next) = self.queue.pop_front() {
            self.submit_text(next);
        }
    }

    /// `/export <word|excel> [path]`: экран диалога в .docx/.xlsx.
    fn do_export(&mut self, input: &str) {
        let mut parts = input.split_whitespace();
        let _ = parts.next(); // сама команда
        let usage = "использование: /export <word|excel> [путь] — экран в .docx/.xlsx";
        let Some(fmt_arg) = parts.next() else {
            self.push_block(ChatBlock::System {
                command: "export".into(),
                text: usage.into(),
            });
            return;
        };
        let Some(format) = crate::export::ExportFormat::parse(fmt_arg) else {
            self.push_block(ChatBlock::System {
                command: "export".into(),
                text: format!("неизвестный формат «{fmt_arg}»; {usage}"),
            });
            return;
        };
        let path = parts.next().map_or_else(
            || {
                let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
                self.tool_ctx
                    .cwd
                    .join(format!("arch-screen-{ts}.{}", format.extension()))
            },
            |p| self.tool_ctx.resolve(p),
        );
        match crate::export::export_blocks(&self.blocks, format, &path) {
            Ok(n) => {
                self.push_block(ChatBlock::System {
                    command: "export".into(),
                    text: format!("экран экспортирован ({n} строк) → {}", path.display()),
                });
            }
            Err(e) => {
                self.push_block(ChatBlock::Error(format!("экспорт не удался: {e}")));
            }
        }
    }

    /// Запускает ход агента в фоновой задаче (сессия переезжает в задачу).
    fn start_turn(&mut self, input: String) {
        let Some(tx) = self.msg_tx.clone() else {
            self.push_block(ChatBlock::Error(
                "внутренняя ошибка: канал событий не подключён".into(),
            ));
            return;
        };
        let Some(session) = self.session.take() else {
            self.push_block(ChatBlock::Error("сессия занята — дождитесь ответа".into()));
            return;
        };
        self.thinking = true;
        self.turn_got_output = false;
        // Fire-and-forget: JoinHandle не храним — результат и ошибки приходят
        // сообщением TurnFinished; при выходе runtime отменит задачу.
        tokio::spawn(async move {
            let (ev_tx, mut ev_rx) = mpsc::channel::<AgentEvent>(AGENT_EVENTS_CAP);
            let fwd_tx = tx.clone();
            let forwarder = tokio::spawn(async move {
                while let Some(ev) = ev_rx.recv().await {
                    if fwd_tx.send(AppMessage::AgentEvent(ev)).await.is_err() {
                        break;
                    }
                }
            });
            let mut session = session;
            let result = session.send(&input, Some(ev_tx)).await;
            // ev_tx дропнут вместе с future send — forwarder дочитает буфер
            // и завершится сам.
            let _ = forwarder.await;
            let _ = tx.send(AppMessage::TurnFinished { session, result }).await;
        });
    }

    /// Запускает слэш-команду в фоновой задаче (сессия переезжает в задачу).
    fn start_slash(&mut self, input: String) {
        let Some(tx) = self.msg_tx.clone() else {
            self.push_block(ChatBlock::Error(
                "внутренняя ошибка: канал событий не подключён".into(),
            ));
            return;
        };
        let Some(session) = self.session.take() else {
            self.push_block(ChatBlock::Error("сессия занята — дождитесь ответа".into()));
            return;
        };
        self.thinking = true;
        self.pending_slash = Some(input.clone());
        let ctx = self.tool_ctx.clone();
        // Fire-and-forget: исход приходит сообщением SlashFinished.
        tokio::spawn(async move {
            let mut session = session;
            let result = slash::execute(&input, &mut session, &ctx).await;
            let _ = tx.send(AppMessage::SlashFinished { session, result }).await;
        });
    }

    /// Обработка сообщения от фоновых задач.
    pub(crate) fn handle_message(&mut self, msg: AppMessage) {
        match msg {
            AppMessage::AgentEvent(ev) => self.handle_agent_event(ev),
            AppMessage::AskUser(req) => self.open_ask(req),
            AppMessage::TurnFinished { session, result } => {
                self.thinking = false;
                self.assistant_open = false;
                self.on_session_back(session);
                match result {
                    Ok(text) => {
                        // Без стриминга дельт не было — показываем финальный текст.
                        if !self.turn_got_output && !text.trim().is_empty() {
                            self.push_block(ChatBlock::Assistant(text));
                        }
                    }
                    Err(e) => self.push_block(ChatBlock::Error(format!(
                        "ход завершился ошибкой: {e}"
                    ))),
                }
                // Очередь: следующее набранное во время хода сообщение.
                self.maybe_start_queued();
            }
            AppMessage::SlashFinished { session, result } => {
                self.thinking = false;
                self.on_session_back(session);
                let command = self.pending_slash.take().unwrap_or_default();
                match result {
                    Ok(slash::SlashOutcome::Handled(text)) => {
                        self.route_slash_output(&command, &text);
                        self.push_block(ChatBlock::System { command, text });
                    }
                    Ok(slash::SlashOutcome::PickModel) => self.open_model_picker(),
                    Ok(slash::SlashOutcome::NewSession) => {
                        // /new: чистый лист — блоки, вкладки и скролл сброшены;
                        // сессия уже ротирована исполнителем (новый журнал).
                        self.blocks.clear();
                        self.panels = Panels::default();
                        self.scroll = 0;
                        self.stick = true;
                        self.push_block(ChatBlock::System {
                            command,
                            text: "новая сессия: история и панели очищены, \
                                   журнал начат заново; прошлые сессии — /sessions"
                                .into(),
                        });
                    }
                    Ok(slash::SlashOutcome::Quit) => self.should_quit = true,
                    Ok(slash::SlashOutcome::Unknown(cmd)) => {
                        self.push_block(ChatBlock::Error(format!(
                            "неизвестная команда: {cmd} (/help — список)"
                        )));
                    }
                    Ok(slash::SlashOutcome::NotSlash) => {
                        self.push_block(ChatBlock::Error(
                            "внутренняя ошибка: NotSlash дошёл до обработчика слэш-команд"
                                .into(),
                        ));
                    }
                    Err(e) => self.push_block(ChatBlock::Error(format!(
                        "команда не удалась: {e}"
                    ))),
                }
                // Очередь: следующее набранное, пока шла команда.
                self.maybe_start_queued();
            }
        }
    }

    /// Обработка события агентного цикла.
    fn handle_agent_event(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::Delta(delta) => {
                self.turn_got_output = true;
                if !self.assistant_open {
                    self.push_block(ChatBlock::Assistant(String::new()));
                    self.assistant_open = true;
                }
                if let Some(ChatBlock::Assistant(text)) = self.blocks.last_mut() {
                    text.push_str(&delta);
                }
            }
            AgentEvent::ToolStart { name, .. } => {
                self.assistant_open = false;
                self.push_block(ChatBlock::Tool {
                    name,
                    state: ToolState::Running,
                    summary: String::new(),
                });
            }
            AgentEvent::ToolEnd {
                name,
                is_error,
                summary,
                content,
            } => {
                self.finish_tool(&name, is_error, &summary);
                self.route_tool_output(&name, &content);
            }
            AgentEvent::Note(text) => {
                self.assistant_open = false;
                self.push_block(ChatBlock::System {
                    command: "детектор".into(),
                    text,
                });
            }
            AgentEvent::TurnDone => self.assistant_open = false,
            // Живое обновление индикатора контекста по ходу длинного хода.
            AgentEvent::ContextUsage(used) => self.history_tokens = used,
        }
        if self.stick {
            self.scroll = 0;
        }
    }

    /// Завершает последний активный tool-блок (по имени, иначе любой Running).
    fn finish_tool(&mut self, name: &str, is_error: bool, summary: &str) {
        let state = if is_error {
            ToolState::Error
        } else {
            ToolState::Ok
        };
        let idx = self
            .blocks
            .iter()
            .rposition(|b| {
                matches!(
                    b,
                    ChatBlock::Tool {
                        name: n,
                        state: ToolState::Running,
                        ..
                    } if n == name
                )
            })
            .or_else(|| {
                self.blocks.iter().rposition(|b| {
                    matches!(
                        b,
                        ChatBlock::Tool {
                            state: ToolState::Running,
                            ..
                        }
                    )
                })
            });
        match idx {
            Some(i) => {
                if let Some(ChatBlock::Tool {
                    state: s,
                    summary: sum,
                    ..
                }) = self.blocks.get_mut(i)
                {
                    *s = state;
                    *sum = summary.to_string();
                }
            }
            None => self.push_block(ChatBlock::Tool {
                name: name.to_string(),
                state,
                summary: summary.to_string(),
            }),
        }
    }

    /// Привязывает вывод инструмента к вкладке правой панели.
    fn route_tool_output(&mut self, name: &str, summary: &str) {
        if name.contains("mermaid") {
            self.panels.mermaid = summary.to_string();
        } else if name.contains("rubric") {
            self.panels.rubric = summary.to_string();
        } else if name.starts_with("kb") || name.starts_with("web") {
            self.panels.knowledge = summary.to_string();
        }
    }

    /// Привязывает результат слэш-команды к вкладке правой панели.
    fn route_slash_output(&mut self, command: &str, text: &str) {
        let head = command.split_whitespace().next().unwrap_or("");
        match head {
            "/mermaid" => self.panels.mermaid = text.to_string(),
            "/rubric" => self.panels.rubric = text.to_string(),
            "/kb" | "/web" | "/fetch" | "/sites" => {
                self.panels.knowledge = text.to_string();
            }
            _ => {}
        }
    }

    /// Возвращает сессию из фоновой задачи; обновляет модель и токены.
    fn on_session_back(&mut self, session: AgentSession) {
        let mut name = session.model_name();
        // Индикатор ризонинга в бейдже модели: 🧠 — on, 🧠off — выключен явно.
        match session.thinking() {
            Some(true) => name.push_str(" 🧠"),
            Some(false) => name.push_str(" 🧠off"),
            None => {}
        }
        if !name.is_empty() {
            self.model_name = name;
        }
        self.history_tokens = session
            .messages()
            .iter()
            .map(ChatMessage::rough_tokens)
            .sum();
        // Бюджет перечитываем: `/model` мог сменить провайдера и окно.
        self.context_budget = session.effective_context_budget();
        self.session = Some(session);
    }

    /// Добавляет блок в чат, удерживая историю в пределах [`MAX_BLOCKS`].
    pub(crate) fn push_block(&mut self, block: ChatBlock) {
        self.blocks.push(block);
        if self.blocks.len() > MAX_BLOCKS {
            let excess = self.blocks.len() - MAX_BLOCKS;
            self.blocks.drain(..excess);
        }
        if self.stick {
            self.scroll = 0;
        }
    }

    /// Размер «страницы» прокрутки.
    fn page(&self) -> usize {
        self.viewport.max(1)
    }

    /// Прокрутка вверх на `n` строк.
    pub(crate) fn scroll_by(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_add(n);
        self.stick = false;
    }

    /// Прокрутка вниз на `n` строк; на дне — снова прилипнуть.
    pub(crate) fn scroll_back(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
        if self.scroll == 0 {
            self.stick = true;
        }
    }

    /// Прилипнуть к низу чата.
    fn scroll_to_bottom(&mut self) {
        self.scroll = 0;
        self.stick = true;
    }
}

/// Отправляет ответ ожидающему инструменту (oneshot; приёмник мог уйти —
/// тогда вопрос мёртв и отвечать некому, это не ошибка).
fn answer(reply: Option<tokio::sync::oneshot::Sender<String>>, text: String) {
    if let Some(tx) = reply {
        let _ = tx.send(text);
    }
}

/// Системный промпт: шаблон `architect` из библиотеки или встроенный.
fn system_prompt(cfg: &Config) -> String {
    let dir = cfg.paths.prompts_dir();
    if let Ok(lib) = prompts::load_library(&dir) {
        if let Some(tpl) = lib.iter().find(|t| t.name == "architect") {
            return tpl.body.clone();
        }
    }
    FALLBACK_SYSTEM_PROMPT.into()
}

/// Заглушки для headless-тестов (без терминала, сети и LLM).
#[cfg(test)]
pub(crate) mod testing {
    use std::path::PathBuf;
    use std::sync::Arc;

    use async_trait::async_trait;

    use crate::agent::AgentSession;
    use crate::config::Config;
    use crate::error::Result;
    use crate::llm::{ChatMessage, ChatRequest, LlmProvider};
    use crate::tool::{ToolContext, ToolRegistry};

    use super::App;

    /// LLM-провайдер-заглушка: отвечает фиксированным текстом, сеть не нужна.
    #[derive(Debug)]
    struct StubProvider;

    #[async_trait]
    impl LlmProvider for StubProvider {
        fn name(&self) -> &str {
            "stub"
        }

        fn model(&self) -> &str {
            "stub-model"
        }

        async fn complete(&self, _req: ChatRequest) -> Result<ChatMessage> {
            Ok(ChatMessage::assistant("ответ-заглушка", Vec::new()))
        }
    }

    /// Сессия на провайдере-заглушке (конструктор реальный, без сети).
    /// Журнал — во временный каталог теста: дефолтный `paths.sessions_dir`
    /// указывает в настоящий ~/.arch-harness, тесты не должны туда писать.
    pub(crate) fn stub_session(cfg: &Arc<Config>) -> AgentSession {
        let mut test_cfg = cfg.as_ref().clone();
        test_cfg.paths.sessions_dir = std::env::temp_dir()
            .join(format!("arch-test-sessions-{}", std::process::id()));
        let cfg = Arc::new(test_cfg);
        let ctx = ToolContext::new(PathBuf::from("/tmp"), cfg.clone());
        AgentSession::new(
            cfg,
            Arc::new(StubProvider),
            ToolRegistry::new(),
            ctx,
            "системный промпт".into(),
        )
    }

    /// Приложение для тестов: сессия-заглушка, без MCP и канала событий.
    pub(crate) fn test_app() -> App {
        let cfg = Arc::new(Config::default());
        let session = stub_session(&cfg);
        let ctx = ToolContext::new(PathBuf::from("/tmp"), cfg);
        App::new(Some(session), ctx, None, None)
    }

    /// Подменяет показания контекста для тестов статус-бара.
    pub(crate) fn set_context_usage(app: &mut App, used: usize, budget: usize) {
        app.history_tokens = used;
        app.context_budget = budget;
    }

    /// Подменяет флаг «модель думает» для тестов очереди ввода.
    pub(crate) fn set_thinking(app: &mut App, thinking: bool) {
        app.thinking = thinking;
    }
}

#[cfg(test)]
mod tests {
    use super::testing;
    use super::testing::{stub_session, test_app};
    use super::*;
    use crate::agent::slash::SlashOutcome;
    use crate::error::HarnessError;

    #[test]
    fn model_picker_lists_config_models_with_think_marks() {
        let mut app = test_app();
        app.open_model_picker();
        let ask = app.ask.as_ref().expect("пикер открыт");
        assert_eq!(ask.kind, super::AskKind::ModelPicker);
        // Дефолтный конфиг: deepseek*, kimi и ряд GLM (BTreeMap, ≥4 моделей).
        assert!(ask.options.len() >= 4, "options: {:?}", ask.options.len());
        let ds = ask
            .options
            .iter()
            .find(|o| o.label == "deepseek")
            .expect("deepseek в списке");
        assert!(ds.description.contains("deepseek-v4-flash"), "id модели: {}", ds.description);
        assert!(ds.description.contains("/think"), "метка ризонинга: {}", ds.description);
        assert!(ask.options.iter().any(|o| o.label == "deepseek-pro"));
        assert!(ask.reply.is_none(), "пикер без канала инструмента");
    }

    #[tokio::test]
    async fn model_picker_answer_dispatches_model_switch_and_esc_cancels() {
        let mut app = test_app();
        let (tx, _rx) = mpsc::channel(8);
        app.attach(tx);
        app.open_model_picker();
        app.answer_ask(Some("kimi".into()));
        assert!(app.ask.is_none(), "модалка закрыта");
        assert_eq!(app.pending_slash.as_deref(), Some("/model kimi"));
        assert!(app.thinking, "переключение пошло в фон");
        // Esc — тихая отмена без диспатча.
        app.thinking = false;
        app.pending_slash = None;
        app.open_model_picker();
        app.answer_ask(None);
        assert!(app.pending_slash.is_none(), "Esc не диспатчит");
        assert!(app.ask.is_none());
    }

    #[test]
    fn history_navigates_up_and_down_with_draft() {
        let mut input = InputState::default();
        input.set_text("первая".into());
        let _ = input.submit();
        input.set_text("вторая".into());
        let _ = input.submit();
        input.set_text("черновик".into());
        input.history_up();
        assert_eq!(input.text(), "вторая");
        input.history_up();
        assert_eq!(input.text(), "первая");
        input.history_up();
        assert_eq!(input.text(), "первая", "дальше начала истории не идём");
        input.history_down();
        assert_eq!(input.text(), "вторая");
        input.history_down();
        assert_eq!(input.text(), "черновик", "за концом истории — черновик");
    }

    #[test]
    fn history_ignores_empty_and_duplicates() {
        let mut input = InputState::default();
        let _ = input.submit();
        assert!(input.history.is_empty());
        input.set_text("команда".into());
        let _ = input.submit();
        input.set_text("команда".into());
        let _ = input.submit();
        assert_eq!(input.history.len(), 1, "дубликат подряд не сохраняется");
    }

    #[test]
    fn tab_completes_and_stays_on_single_candidate() {
        let mut input = InputState::default();
        input.set_text("/me".into());
        assert!(input.complete_tab());
        assert_eq!(input.text(), "/mermaid");
        assert!(input.complete_tab(), "Tab на полной команде остаётся в цикле");
        assert_eq!(input.text(), "/mermaid");
    }

    #[test]
    fn tab_without_candidates_returns_false() {
        let mut input = InputState::default();
        input.set_text("/zzz".into());
        assert!(!input.complete_tab());
        input.set_text("привет".into());
        assert!(!input.complete_tab());
    }

    #[test]
    fn ghost_hint_shows_candidate_suffix() {
        let mut input = InputState::default();
        input.set_text("/mer".into());
        assert_eq!(input.ghost_hint().as_deref(), Some("maid"));
        input.set_text("текст".into());
        assert_eq!(input.ghost_hint(), None);
    }

    #[test]
    fn editing_handles_unicode_boundaries() {
        let mut input = InputState::default();
        for c in "при".chars() {
            input.insert_char(c);
        }
        assert_eq!(input.text(), "при");
        input.move_left();
        input.backspace();
        assert_eq!(input.text(), "пи", "backspace удалил «р» перед курсором");
        input.move_home();
        input.delete();
        assert_eq!(input.text(), "и", "delete удалил «п» под курсором");
        input.move_end();
        input.insert_char('!');
        assert_eq!(input.text(), "и!");
    }

    #[test]
    fn mouse_wheel_scrolls_dialog() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut app = test_app();
        app.screen = Screen::Chat;
        let mouse = |kind| MouseEvent {
            kind,
            column: 5,
            row: 5,
            modifiers: crossterm::event::KeyModifiers::empty(),
        };
        app.handle_mouse(mouse(MouseEventKind::ScrollUp));
        assert_eq!(app.scroll, 3, "колесо вверх — три строки");
        assert!(!app.stick, "скролл отлипает от дна");
        app.handle_mouse(mouse(MouseEventKind::ScrollUp));
        assert_eq!(app.scroll, 6);
        app.handle_mouse(mouse(MouseEventKind::ScrollDown));
        app.handle_mouse(mouse(MouseEventKind::ScrollDown));
        assert_eq!(app.scroll, 0, "колесо вниз — назад к дну");
        assert!(app.stick, "на дне — прилипание");
        // Вне чата колесо игнорируется.
        app.screen = Screen::Splash;
        app.handle_mouse(mouse(MouseEventKind::ScrollUp));
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn click_on_jump_button_scrolls_to_bottom() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut app = test_app();
        app.screen = Screen::Chat;
        app.scroll_by(9);
        assert!(app.scroll > 0 && !app.stick);
        // Кнопка « ▼ » 3×1 (координаты ставит рендер — здесь эмулируем).
        app.jump_btn = Some(ratatui::layout::Rect::new(10, 20, 3, 1));
        let click = |col, row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: crossterm::event::KeyModifiers::empty(),
        };
        // Клик мимо кнопки — скролл не сбрасывается.
        app.handle_mouse(click(0, 0));
        assert_eq!(app.scroll, 9, "клик мимо кнопки — без эффекта");
        // Клик по кнопке (в т.ч. по крайней ячейке) — прыжок к дну.
        app.handle_mouse(click(12, 20));
        assert_eq!(app.scroll, 0, "клик по ▼ — к свежему ответу");
        assert!(app.stick, "снова прилипли к дну");
    }

    #[test]
    fn agent_events_build_blocks_and_update_panels() {
        let mut app = test_app();
        app.handle_message(AppMessage::AgentEvent(AgentEvent::Delta("При".into())));
        app.handle_message(AppMessage::AgentEvent(AgentEvent::Delta("вет".into())));
        app.handle_message(AppMessage::AgentEvent(AgentEvent::ToolStart {
            name: "mermaid_render".into(),
            args: serde_json::json!({}),
        }));
        app.handle_message(AppMessage::AgentEvent(AgentEvent::ToolEnd {
            name: "mermaid_render".into(),
            is_error: false,
            summary: "┌───┐".into(),
            content: "┌───┐\n│ A │\n└───┘".into(),
        }));
        app.handle_message(AppMessage::AgentEvent(AgentEvent::TurnDone));
        match app.blocks.first() {
            Some(ChatBlock::Assistant(text)) => assert_eq!(text, "Привет"),
            other => panic!("ожидался assistant-блок, получено: {other:?}"),
        }
        match app.blocks.get(1) {
            Some(ChatBlock::Tool {
                state: ToolState::Ok,
                summary,
                ..
            }) => assert_eq!(summary, "┌───┐"),
            other => panic!("ожидался завершённый tool-блок, получено: {other:?}"),
        }
        assert_eq!(
            app.panels.mermaid, "┌───┐\n│ A │\n└───┘",
            "на вкладку Mermaid уходит ПОЛНЫЙ рендер, не summary"
        );
    }

    #[test]
    fn turn_error_pushes_error_block_and_restores_session() {
        let mut app = test_app();
        let session = app.session.take().expect("сессия есть");
        app.thinking = true;
        app.handle_message(AppMessage::TurnFinished {
            session,
            result: Err(HarnessError::Agent("llm down".into())),
        });
        assert!(!app.thinking);
        assert!(app.session.is_some(), "сессия вернулась в App");
        assert!(matches!(app.blocks.last(), Some(ChatBlock::Error(_))));
    }

    #[test]
    fn turn_without_deltas_shows_final_text() {
        let mut app = test_app();
        let session = app.session.take().expect("сессия есть");
        app.thinking = true;
        app.handle_message(AppMessage::TurnFinished {
            session,
            result: Ok("финальный ответ".into()),
        });
        assert!(matches!(
            app.blocks.last(),
            Some(ChatBlock::Assistant(t)) if t == "финальный ответ"
        ));
    }

    #[test]
    fn slash_handled_pushes_system_block_and_updates_panel() {
        let mut app = test_app();
        let session = app.session.take().expect("сессия есть");
        app.pending_slash = Some("/mermaid graph TD; A-->B".into());
        app.thinking = true;
        app.handle_message(AppMessage::SlashFinished {
            session,
            result: Ok(SlashOutcome::Handled("ASCII-art".into())),
        });
        assert!(!app.thinking);
        assert!(app.session.is_some());
        assert_eq!(app.panels.mermaid, "ASCII-art");
        match app.blocks.last() {
            Some(ChatBlock::System { command, text }) => {
                assert!(command.starts_with("/mermaid"));
                assert_eq!(text, "ASCII-art");
            }
            other => panic!("ожидался system-блок, получено: {other:?}"),
        }
    }

    #[test]
    fn slash_quit_sets_should_quit() {
        let mut app = test_app();
        let session = app.session.take().expect("сессия есть");
        app.handle_message(AppMessage::SlashFinished {
            session,
            result: Ok(SlashOutcome::Quit),
        });
        assert!(app.should_quit);
    }

    #[test]
    fn slash_new_session_clears_blocks_panels_and_scroll() {
        let mut app = test_app();
        app.push_block(ChatBlock::User("старое сообщение".into()));
        app.panels.mermaid = "старый арт".into();
        app.scroll_by(3);
        let session = app.session.take().expect("сессия есть");
        app.handle_message(AppMessage::SlashFinished {
            session,
            result: Ok(SlashOutcome::NewSession),
        });
        assert_eq!(app.blocks.len(), 1, "осталась только системная заметка");
        match app.blocks.last() {
            Some(ChatBlock::System { text, .. }) => {
                assert!(text.contains("новая сессия"), "{text}")
            }
            other => panic!("ожидался system-блок, получено: {other:?}"),
        }
        assert!(app.panels.mermaid.is_empty(), "вкладки очищены");
        assert_eq!(app.scroll, 0);
        assert!(app.stick, "прилипание к дну восстановлено");
        assert_eq!(app.history_tokens(), 0, "индикатор контекста сброшен");
    }

    #[tokio::test]
    async fn finished_turn_updates_context_gauge() {
        let mut app = test_app();
        let mut session = app.session.take().expect("сессия есть");
        // Полный ход через реальный AgentSession::send (провайдер-заглушка).
        session.send("расскажи про сагу", None).await.expect("ход");
        assert!(!session.messages().is_empty());
        app.handle_message(AppMessage::TurnFinished {
            session,
            result: Ok("ответ-заглушка".into()),
        });
        assert!(
            app.history_tokens() > 0,
            "после хода счётчик контекста обязан вырасти"
        );
    }

    #[test]
    fn context_usage_event_updates_gauge_mid_turn() {
        let mut app = test_app();
        app.handle_message(AppMessage::AgentEvent(AgentEvent::ContextUsage(12_345)));
        assert_eq!(app.history_tokens(), 12_345, "индикатор обновился по ходу хода");
        app.handle_message(AppMessage::AgentEvent(AgentEvent::ContextUsage(20_000)));
        assert_eq!(app.history_tokens(), 20_000);
    }

    #[test]
    fn typing_and_enter_enqueue_while_agent_thinks() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        testing::set_thinking(&mut app, true);
        for c in "добавь NFR".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(app.input.text(), "добавь NFR", "ввод во время хода работает");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.queue.len(), 1, "сообщение в очереди");
        assert_eq!(app.queue[0], "добавь NFR");
        assert!(app.input.text().is_empty(), "строка ввода очищена");
        assert!(app.thinking, "новый ход не начался — сообщение ждёт");
        assert!(app.session.is_some(), "сессия не уехала в фоновую задачу");
    }

    #[test]
    fn alt_enter_and_bang_prefix_jump_to_queue_front() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        testing::set_thinking(&mut app, true);
        app.input.set_text("обычное".into());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.input.set_text("срочное".into());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        app.input.set_text("!!ещё срочнее".into());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let order: Vec<&str> = app.queue.iter().map(String::as_str).collect();
        assert_eq!(
            order,
            vec!["ещё срочнее", "срочное", "обычное"],
            "срочные — в начало очереди (Alt+Enter и «!!»)"
        );
    }

    #[tokio::test]
    async fn queued_message_starts_when_turn_finishes() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        let (tx, _rx) = mpsc::channel(8);
        app.msg_tx = Some(tx);
        testing::set_thinking(&mut app, true);
        app.queue.push_back("второй вопрос".into());
        let session = app.session.take().expect("сессия есть");
        app.handle_message(AppMessage::TurnFinished {
            session,
            result: Ok("первый ответ".into()),
        });
        assert!(app.thinking, "сразу стартовал ход из очереди");
        assert!(app.queue.is_empty(), "очередь опустела");
        assert!(
            app.blocks
                .iter()
                .any(|b| matches!(b, ChatBlock::User(t) if t == "второй вопрос")),
            "блок пользователя для очередного сообщения"
        );
    }

    #[tokio::test]
    async fn queued_slash_runs_after_turn() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        let (tx, _rx) = mpsc::channel(8);
        app.msg_tx = Some(tx);
        testing::set_thinking(&mut app, true);
        app.queue.push_back("/tools".into());
        let session = app.session.take().expect("сессия есть");
        app.handle_message(AppMessage::TurnFinished {
            session,
            result: Ok("ответ".into()),
        });
        assert!(app.thinking);
        assert_eq!(
            app.pending_slash.as_deref(),
            Some("/tools"),
            "очередная слэш-команда запущена"
        );
    }

    #[test]
    fn slash_unknown_pushes_error_block() {
        let mut app = test_app();
        let session = app.session.take().expect("сессия есть");
        app.handle_message(AppMessage::SlashFinished {
            session,
            result: Ok(SlashOutcome::Unknown("/zzz".into())),
        });
        match app.blocks.last() {
            Some(ChatBlock::Error(text)) => assert!(text.contains("/zzz")),
            other => panic!("ожидался error-блок, получено: {other:?}"),
        }
    }

    #[test]
    fn slash_not_slash_is_internal_error() {
        let mut app = test_app();
        let session = app.session.take().expect("сессия есть");
        app.handle_message(AppMessage::SlashFinished {
            session,
            result: Ok(SlashOutcome::NotSlash),
        });
        match app.blocks.last() {
            Some(ChatBlock::Error(text)) => assert!(text.contains("NotSlash")),
            other => panic!("ожидался error-блок, получено: {other:?}"),
        }
    }

    #[test]
    fn slash_execution_error_pushes_error_block() {
        let mut app = test_app();
        let session = app.session.take().expect("сессия есть");
        app.handle_message(AppMessage::SlashFinished {
            session,
            result: Err(HarnessError::Agent("boom".into())),
        });
        assert!(matches!(app.blocks.last(), Some(ChatBlock::Error(_))));
    }

    #[test]
    fn scroll_sticks_to_bottom_until_scrolled_up() {
        let mut app = test_app();
        app.viewport = 10;
        app.scroll_by(5);
        assert!(!app.stick);
        app.push_block(ChatBlock::User("x".into()));
        assert_eq!(app.scroll, 5, "без прилипания новые блоки скролл не сбрасывают");
        app.scroll_back(100);
        assert_eq!(app.scroll, 0);
        assert!(app.stick, "на дне снова прилипаем");
    }

    #[test]
    fn key_q_quits_only_on_empty_input() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(app.should_quit, "q на пустом вводе — выход");

        let mut app = test_app();
        app.screen = Screen::Chat;
        app.input.insert_char('п');
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(!app.should_quit, "q в непустом вводе — обычный символ");
        assert_eq!(app.input.text(), "пq");
    }

    #[test]
    fn splash_any_key_enters_chat() {
        let mut app = test_app();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.screen, Screen::Chat));

        let mut app = test_app();
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(app.should_quit, "q на заставке — выход");
    }

    #[test]
    fn session_constructor_keeps_stub_provider() {
        let cfg = Arc::new(Config::default());
        let session = stub_session(&cfg);
        assert!(session.messages().is_empty());
    }

    /// Модалка propose_options с двумя вариантами (без рекомендации).
    fn open_test_ask(app: &mut App) -> tokio::sync::oneshot::Receiver<String> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        app.handle_message(AppMessage::AskUser(AskRequest {
            question: "Какой брокер для событийной шины?".into(),
            options: vec![
                crate::tool::AskOption {
                    label: "Kafka".into(),
                    description: "масштаб, но тяжёлый".into(),
                },
                crate::tool::AskOption {
                    label: "NATS".into(),
                    description: "лёгкий, без хранения".into(),
                },
            ],
            recommended: None,
            reply: reply_tx,
        }));
        reply_rx
    }

    #[test]
    fn ask_modal_opens_navigates_and_enter_confirms() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        let mut rx = open_test_ask(&mut app);
        assert!(app.ask.is_some(), "модалка открыта");
        assert_eq!(app.ask.as_ref().map(|a| a.selected), Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.ask.as_ref().map(|a| a.selected), Some(1), "↓ двигает курсор");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.ask.is_none(), "после Enter модалка закрыта");
        let chosen = rx.try_recv().expect("ответ ушёл инструменту");
        assert_eq!(chosen, "NATS");
    }

    #[test]
    fn ask_modal_esc_declines_with_empty_answer() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        let mut rx = open_test_ask(&mut app);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.ask.is_none());
        assert!(!app.should_quit, "Esc в модалке — отказ, а не выход из TUI");
        let chosen = rx.try_recv().expect("ответ ушёл");
        assert_eq!(chosen, "", "отказ — пустая строка");
    }

    #[test]
    fn ask_modal_digit_quick_selects_and_recommended_preselects() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        let (reply_tx, mut rx) = tokio::sync::oneshot::channel();
        app.handle_message(AppMessage::AskUser(AskRequest {
            question: "q".into(),
            options: vec![
                crate::tool::AskOption { label: "A".into(), description: String::new() },
                crate::tool::AskOption { label: "B".into(), description: String::new() },
            ],
            recommended: Some("B".into()),
            reply: reply_tx,
        }));
        assert_eq!(app.ask.as_ref().map(|a| a.selected), Some(1), "курсор на рекомендации");
        app.handle_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
        assert!(app.ask.is_none());
        assert_eq!(rx.try_recv().expect("ответ"), "A", "цифра выбирает мгновенно");
    }

    #[test]
    fn ask_modal_blocks_plain_typing_while_open() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        let _rx = open_test_ask(&mut app);
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(app.input.text().is_empty(), "обычный ввод заблокирован модалкой");
        assert!(app.ask.is_some(), "нецифровая клавиша модалку не закрывает");
    }

    #[test]
    fn viewer_toggles_and_pans_with_keys() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        app.handle_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE));
        assert!(app.viewer.is_some(), "F4 открывает просмотрщик");
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let v = app.viewer.expect("открыт");
        assert_eq!((v.scroll_x, v.scroll_y), (8, 1), "→/↓ панорамируют");
        // Печать в просмотрщике не уходит в строку ввода.
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(app.input.text().is_empty());
        // Смена вкладки внутри просмотрщика сбрасывает скролл.
        app.handle_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        let v = app.viewer.expect("открыт");
        assert_eq!((v.scroll_x, v.scroll_y), (0, 0), "скролл сброшен на новой вкладке");
        assert!(matches!(app.right_tab(), RightTab::Rubric));
        app.handle_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE));
        assert!(app.viewer.is_none(), "F4 закрывает");
    }

    #[test]
    fn viewer_esc_closes_without_quitting_app() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        app.handle_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.viewer.is_none(), "Esc — назад из просмотрщика");
        assert!(!app.should_quit, "приложение не выходит");
    }

    #[test]
    fn viewer_mouse_wheel_scrolls_viewer_not_chat() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        app.handle_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE));
        let wheel = |kind, modifiers| crossterm::event::MouseEvent {
            kind,
            column: 5,
            row: 5,
            modifiers,
        };
        app.handle_mouse(wheel(
            crossterm::event::MouseEventKind::ScrollDown,
            KeyModifiers::NONE,
        ));
        app.handle_mouse(wheel(
            crossterm::event::MouseEventKind::ScrollDown,
            KeyModifiers::SHIFT,
        ));
        let v = app.viewer.expect("открыт");
        assert_eq!(v.scroll_y, 3, "колесо — вертикаль просмотрщика");
        assert_eq!(v.scroll_x, 8, "Shift+колесо — горизонталь");
        assert_eq!(app.scroll, 0, "скролл чата не тронут");
    }
}
