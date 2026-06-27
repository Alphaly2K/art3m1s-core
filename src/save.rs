//! 存档/读档子系统。
//!
//! 负责将解释器的完整运行状态序列化到文件，以及从文件恢复。
//! 使用 serde_json 作为序列化格式。

use asb_interpreter::CallFrame;
use asb_interpreter::variable::VariableStore;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 存档中的音频重放快照。只记录当前应播放的音频，不记录播放进度；
/// 读档时所有音频都从头重放。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AudioSnapshot {
    #[serde(default)]
    pub bgm: Option<AudioChannelSnapshot>,
    #[serde(default)]
    pub se: Vec<AudioChannelSnapshot>,
    #[serde(default)]
    pub voice: Vec<AudioChannelSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioChannelSnapshot {
    pub id: String,
    pub file: String,
    pub loop_play: bool,
    pub gain: i32,
    pub pan: i32,
    #[serde(default)]
    pub skippable: bool,
}

impl From<&crate::audio::SoundChannel> for AudioChannelSnapshot {
    fn from(channel: &crate::audio::SoundChannel) -> Self {
        Self {
            id: channel.id.clone(),
            file: channel.file.clone(),
            loop_play: channel.loop_play,
            gain: channel.raw_gain,
            pan: channel.raw_pan,
            skippable: channel.skippable,
        }
    }
}

impl AudioSnapshot {
    pub fn from_audio(audio: &dyn crate::audio::AudioBackend) -> Self {
        let state = audio.audio_state();
        let bgm = state
            .bgm_channel
            .as_ref()
            .filter(|channel| channel.playing)
            .map(AudioChannelSnapshot::from);
        let mut se: Vec<_> = state
            .se_channels
            .values()
            .filter(|channel| channel.playing)
            .map(AudioChannelSnapshot::from)
            .collect();
        let mut voice: Vec<_> = state
            .voice_channels
            .values()
            .filter(|channel| channel.playing)
            .map(AudioChannelSnapshot::from)
            .collect();
        se.sort_by(|a, b| a.id.cmp(&b.id));
        voice.sort_by(|a, b| a.id.cmp(&b.id));
        Self { bgm, se, voice }
    }

    pub fn restore_into(&self, audio: &mut dyn crate::audio::AudioBackend) {
        if let Some(bgm) = &self.bgm {
            audio.play_bgm(
                &bgm.file,
                &crate::audio::BgmConfig {
                    loop_play: bgm.loop_play,
                    gain: Some(bgm.gain),
                    pan: Some(bgm.pan),
                    fade_in_ms: 0,
                    buffer_size: None,
                },
            );
        }
        for se in &self.se {
            audio.play_se(
                &se.id,
                &se.file,
                &crate::audio::SeConfig {
                    loop_play: se.loop_play,
                    gain: Some(se.gain),
                    pan: Some(se.pan),
                    fade_in_ms: 0,
                    buffer_size: None,
                    skippable: se.skippable,
                },
            );
        }
        for voice in &self.voice {
            audio.play_voice(
                &voice.id,
                &voice.file,
                &crate::audio::SeConfig {
                    loop_play: voice.loop_play,
                    gain: Some(voice.gain),
                    pan: Some(voice.pan),
                    fade_in_ms: 0,
                    buffer_size: None,
                    skippable: voice.skippable,
                },
            );
        }
    }
}

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
    /// 引擎侧保留模式画面层树。旧存档没有该字段，读档时仅恢复脚本/Lua 状态。
    #[serde(default)]
    pub scene: Option<crate::compositor::Scene>,
    /// 引擎侧音频播放状态。旧存档没有该字段时读档保持静音，等待脚本后续事件。
    #[serde(default)]
    pub audio: Option<AudioSnapshot>,
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
            scene: None,
            audio: None,
        }
    }

    pub fn with_scene(mut self, scene: crate::compositor::Scene) -> Self {
        self.scene = Some(scene);
        self
    }

    pub fn with_audio(mut self, audio: AudioSnapshot) -> Self {
        self.audio = Some(audio);
        self
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{AudioBackend, BgmConfig, SeConfig, AudioStateBackend};
    use asb_interpreter::VariableStore;

    #[test]
    fn save_data_serializes_optional_scene_snapshot() {
        let mut scene = crate::compositor::Scene::new();
        scene.create("1.0", Some("bg/room".to_string()));
        scene.set_props(
            "1.0",
            &std::collections::HashMap::from([("alpha".to_string(), "255".to_string())]),
        );

        let data = SaveData {
            variables: VariableStore::new(),
            current_script: "system/script.asb".to_string(),
            current_line: 38,
            call_stack: Vec::new(),
            scene: Some(scene),
            audio: Some(AudioSnapshot {
                bgm: Some(AudioChannelSnapshot {
                    id: "bgm".to_string(),
                    file: "bgm01.ogg".to_string(),
                    loop_play: true,
                    gain: 800,
                    pan: 0,
                    skippable: false,
                }),
                se: Vec::new(),
                voice: Vec::new(),
            }),
        };
        let json = serde_json::to_string(&data).unwrap();
        let restored: SaveData = serde_json::from_str(&json).unwrap();
        let scene = restored.scene.unwrap();
        assert_eq!(scene.get("1.0").unwrap().file.as_deref(), Some("bg/room"));
        assert_eq!(scene.get("1.0").unwrap().props.alpha, Some(255));
        assert_eq!(
            restored.audio.unwrap().bgm.unwrap().file,
            "bgm01.ogg".to_string()
        );
    }

    #[test]
    fn save_data_accepts_legacy_files_without_scene() {
        let json = r#"{
            "variables": {"local": {}, "global": {}, "system": {}},
            "current_script": "system/script.asb",
            "current_line": 38,
            "call_stack": []
        }"#;
        let restored: SaveData = serde_json::from_str(json).unwrap();
        assert!(restored.scene.is_none());
        assert!(restored.audio.is_none());
    }

    #[test]
    fn audio_snapshot_replays_active_channels_from_start() {
        let mut audio = AudioStateBackend::new();
        audio.play_bgm(
            "bgm/scene.ogg",
            &BgmConfig {
                loop_play: true,
                gain: Some(700),
                pan: Some(-100),
                fade_in_ms: 1000,
                buffer_size: None,
            },
        );
        audio.play_se(
            "amb",
            "se/wind.ogg",
            &SeConfig {
                loop_play: true,
                gain: Some(400),
                pan: Some(100),
                fade_in_ms: 500,
                buffer_size: None,
                skippable: true,
            },
        );
        audio.advance(250);

        let snapshot = AudioSnapshot::from_audio(&audio);
        assert_eq!(snapshot.bgm.as_ref().unwrap().file, "bgm/scene.ogg");
        assert_eq!(snapshot.bgm.as_ref().unwrap().gain, 700);
        assert_eq!(snapshot.se[0].file, "se/wind.ogg");
        assert!(snapshot.se[0].skippable);

        let mut restored = AudioStateBackend::new();
        snapshot.restore_into(&mut restored);

        let state = restored.audio_state();
        let bgm = state.bgm_channel.as_ref().unwrap();
        assert_eq!(bgm.file, "bgm/scene.ogg");
        assert_eq!(
            bgm.current_gain,
            crate::audio::SoundChannel::gain_to_linear(700)
        );
        assert!(bgm.fade.is_none());
        let se = state.se_channels.get("amb").unwrap();
        assert_eq!(se.file, "se/wind.ogg");
        assert_eq!(
            se.current_gain,
            crate::audio::SoundChannel::gain_to_linear(400)
        );
        assert!(se.skippable);
        assert!(se.fade.is_none());
    }
}
