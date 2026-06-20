//! 存档/读档子系统。
//!
//! 负责将解释器的完整运行状态序列化到文件，以及从文件恢复。
//! 使用 serde_json 作为序列化格式。

use asb_interpreter::CallFrame;
use asb_interpreter::variable::VariableStore;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 一个完整的存档。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveData {
    /// 变量存储（local / global / system）
    pub variables: VariableStore,
    /// 当前脚本文件名
    pub current_script: String,
    /// 当前行号
    pub current_line: usize,
    /// 调用栈
    pub call_stack: Vec<CallFrameSnapshot>,
}

/// 调用栈帧快照（不依赖 asb-interpreter 的 CallFrame）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallFrameSnapshot {
    pub script: String,
    pub return_line: usize,
}

impl From<&CallFrame> for CallFrameSnapshot {
    fn from(f: &CallFrame) -> Self {
        Self {
            script: f.script.clone(),
            return_line: f.return_line,
        }
    }
}

impl From<CallFrameSnapshot> for CallFrame {
    fn from(s: CallFrameSnapshot) -> Self {
        Self {
            script: s.script,
            return_line: s.return_line,
        }
    }
}

/// 存档管理器。
pub struct SaveManager {
    /// 存档目录（通常为项目根下的 `save/`）
    save_dir: PathBuf,
}

impl SaveManager {
    /// 创建存档管理器。
    ///
    /// `save_dir` 是存档文件所在的目录，不存在时自动创建。
    pub fn new(save_dir: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&save_dir)?;
        Ok(Self { save_dir })
    }

    /// 保存一个存档到指定文件名。
    pub fn save(&self, file: &str, data: &SaveData) -> std::io::Result<()> {
        let path = self.resolve(file);
        let json = serde_json::to_string_pretty(data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&path, json)
    }

    /// 从指定文件名读取一个存档。
    pub fn load(&self, file: &str) -> std::io::Result<SaveData> {
        let path = self.resolve(file);
        let json = std::fs::read_to_string(&path)?;
        serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// 列出存档目录下所有存档文件。
    pub fn list(&self) -> std::io::Result<Vec<PathBuf>> {
        let mut entries: Vec<_> = std::fs::read_dir(&self.save_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == "dat")
                    .unwrap_or(false)
            })
            .map(|e| e.path())
            .collect();
        entries.sort();
        Ok(entries)
    }

    /// 检查存档文件是否存在。
    pub fn exists(&self, file: &str) -> bool {
        self.resolve(file).exists()
    }

    /// 删除存档文件。
    pub fn delete(&self, file: &str) -> std::io::Result<()> {
        let path = self.resolve(file);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    fn resolve(&self, file: &str) -> PathBuf {
        let name = if file.ends_with(".dat") {
            file.to_string()
        } else {
            format!("{}.dat", file)
        };
        self.save_dir.join(name)
    }
}

impl SaveData {
    /// 从解释器当前状态构建存档数据。
    pub fn from_interpreter(interpreter: &asb_interpreter::Interpreter) -> Self {
        Self {
            variables: interpreter.variables(),
            current_script: interpreter.current_script().unwrap_or("").to_string(),
            current_line: interpreter.current_line(),
            call_stack: interpreter
                .call_stack()
                .iter()
                .map(CallFrameSnapshot::from)
                .collect(),
        }
    }

    /// 将存档数据恢复到解释器。
    pub fn restore(
        &self,
        interpreter: &mut asb_interpreter::Interpreter,
    ) -> asb_interpreter::Result<()> {
        interpreter.restore_variables(self.variables.clone());
        let stack: Vec<CallFrame> = self
            .call_stack
            .iter()
            .cloned()
            .map(CallFrame::from)
            .collect();
        interpreter.restore_position(&self.current_script, self.current_line, stack)
    }
}
