//! Shows what `volsave-ops` reports on the console and in the log.

use volsave_core::Progress;
use volsave_ops::{Level, Reporter, Stage};

use crate::ProgressPrinter;
use crate::log;

#[derive(Default)]
pub(crate) struct Console {
    printer: Option<(Stage, ProgressPrinter)>,
}

impl Reporter for Console {
    fn message(&mut self, level: Level, text: &str) {
        match level {
            Level::Result => log::say(text),
            Level::Status => log::note(text),
            Level::Warning => log::warn(text),
        }
    }

    fn progress(&mut self, stage: Stage, progress: &Progress) {
        if self.printer.as_ref().is_none_or(|(s, _)| *s != stage) {
            self.stage_done(stage);
            self.printer = Some((stage, ProgressPrinter::new(stage.label())));
        }
        if let Some((_, printer)) = &mut self.printer {
            printer.update(progress);
        }
    }

    fn stage_done(&mut self, _stage: Stage) {
        if let Some((_, printer)) = self.printer.take() {
            printer.finish();
        }
    }
}
