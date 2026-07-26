//! 存档相关标签处理器
//!
//! 实现 syssave, se_saveok, se_loadok 等标签。

use super::{ExecutionContext, TagHandler, TagResult};
use crate::error::Result;
use crate::event::{Event, SaveAction};

/// [syssave] 系统存档标签
pub struct SysSaveHandler;

impl TagHandler for SysSaveHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Emit(Event::SaveOperation {
            action: SaveAction::SystemSave,
        }))
    }
}

/// [se_saveok] 存档完成音效标签
pub struct SeSaveOkHandler;

impl TagHandler for SeSaveOkHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let wait = ctx.instruction.get("0") == Some("wait");
        Ok(TagResult::Emit(Event::PlaySound {
            name: "saveok".to_string(),
            wait,
        }))
    }
}

/// [se_loadok] 读档完成音效标签
pub struct SeLoadOkHandler;

impl TagHandler for SeLoadOkHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let wait = ctx.instruction.get("0") == Some("wait");
        Ok(TagResult::Emit(Event::PlaySound {
            name: "loadok".to_string(),
            wait,
        }))
    }
}

/// [se_exitok] 退出确认音效标签
pub struct SeExitOkHandler;

impl TagHandler for SeExitOkHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let wait = ctx.instruction.get("0") == Some("wait");
        Ok(TagResult::Emit(Event::PlaySound {
            name: "exitok".to_string(),
            wait,
        }))
    }
}

/// [allsoundstop] 停止所有音效标签
pub struct AllSoundStopHandler;

impl TagHandler for AllSoundStopHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let duration = ctx
            .instruction
            .get("time")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        Ok(TagResult::Emit(Event::StopAllSounds { duration }))
    }
}
