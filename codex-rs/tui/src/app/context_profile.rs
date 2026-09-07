//! Inserts the `/ctx` context breakdown card. The profiler lives on `App`, so the card is built
//! here rather than in the widget that dispatched the command.

use codex_context_profiler::ProfilerState;

use super::App;
use crate::context_profiler::build;
use crate::context_profiler::new_context_card_cell;

impl App {
    pub(super) fn show_context_profile(&mut self) {
        let thread_id = self.current_displayed_thread_id();
        let card = match thread_id.and_then(|id| self.profiler.state(&id)) {
            Some(state) => build(state),
            None => build(&ProfilerState::default()),
        };
        self.chat_widget.add_to_history(new_context_card_cell(card));
    }
}
