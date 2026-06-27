//! 存根视频后端。
//!
//! 不产生任何视频输出的存根实现，用于测试和不支持视频的环境。
//! 所有视频操作立即完成，触发完成事件处理器。

use crate::video::engine::*;
use std::collections::VecDeque;

/// 存根视频后端。
///
/// 不实际解码或渲染视频，但会正确触发完成事件，
/// 让脚本可以继续执行（等价于视频瞬间播放完毕）。
#[derive(Debug, Default)]
pub struct StubVideoBackend {
    state: VideoState,
    finish_queue: VecDeque<VideoFinishEvent>,
}

impl StubVideoBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

impl VideoBackend for StubVideoBackend {
    fn play_fullscreen(&mut self, config: &VideoConfig) {
        // 停止旧的全屏视频
        self.stop_fullscreen();

        let mut channel = VideoChannel::new("__fullscreen__", &config.file);
        channel.loop_play = config.loop_play;
        channel.skippable = config.skippable;
        channel.playing = true;

        self.state.fullscreen_video = Some(channel);

        // 存根模式：非循环视频立即完成
        if !config.loop_play {
            let handler = self.state.finish_handler.clone();
            self.finish_queue
                .push_back(VideoFinishEvent { id: None, handler });
        }
    }

    fn stop_fullscreen(&mut self) -> bool {
        if self.state.fullscreen_video.take().is_some() {
            true
        } else {
            false
        }
    }

    fn is_fullscreen_playing(&self) -> bool {
        self.state
            .fullscreen_video
            .as_ref()
            .map_or(false, |v| v.playing)
    }

    fn play_layer(&mut self, id: &str, config: &VideoConfig) {
        let mut channel = VideoChannel::new(id, &config.file);
        channel.loop_play = config.loop_play;
        channel.skippable = config.skippable;
        channel.playing = true;

        self.state.video_layers.insert(id.to_string(), channel);

        // 存根模式：非循环视频立即完成
        if !config.loop_play {
            let handler = self.state.finish_handler.clone();
            self.finish_queue.push_back(VideoFinishEvent {
                id: Some(id.to_string()),
                handler,
            });
        }
    }

    fn stop_layer(&mut self, id: &str) -> bool {
        self.state.video_layers.remove(id).is_some()
    }

    fn is_layer_playing(&self, id: &str) -> bool {
        self.state.video_layers.get(id).map_or(false, |v| v.playing)
    }

    fn stop_all_videos(&mut self) {
        self.state.fullscreen_video = None;
        self.state.video_layers.clear();
    }

    fn set_finish_handler(&mut self, handler: VideoFinishHandler) {
        self.state.finish_handler = Some(handler);
    }

    fn remove_finish_handler(&mut self) {
        self.state.finish_handler = None;
    }

    fn advance(&mut self, delta_ms: u64) {
        self.state.clock_ms += delta_ms;

        // 存根模式：不实际推进视频，因为视频已经"瞬间完成"
        // 但需要清理已完成的视频
        if let Some(ref mut video) = self.state.fullscreen_video {
            if video.playing && !video.loop_play {
                video.playing = false;
            }
        }

        for channel in self.state.video_layers.values_mut() {
            if channel.playing && !channel.loop_play {
                channel.playing = false;
            }
        }
    }

    fn poll_finish_events(&mut self) -> Vec<VideoFinishEvent> {
        self.finish_queue.drain(..).collect()
    }

    fn video_state(&self) -> &VideoState {
        &self.state
    }

    fn video_state_mut(&mut self) -> &mut VideoState {
        &mut self.state
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stub_fullscreen_video() {
        let mut backend = StubVideoBackend::new();

        let config = VideoConfig {
            file: "test.mpg".to_string(),
            skippable: true,
            loop_play: false,
            delay_margin_ms: None,
        };

        backend.play_fullscreen(&config);
        assert!(backend.is_fullscreen_playing());

        // 存根模式：非循环视频立即产生完成事件
        let events = backend.poll_finish_events();
        assert_eq!(events.len(), 1);
        assert!(events[0].id.is_none());

        // advance 后视频停止
        backend.advance(16);
        assert!(!backend.is_fullscreen_playing());
    }

    #[test]
    fn test_stub_loop_video() {
        let mut backend = StubVideoBackend::new();

        let config = VideoConfig {
            file: "test.mpg".to_string(),
            skippable: true,
            loop_play: true,
            delay_margin_ms: None,
        };

        backend.play_fullscreen(&config);
        assert!(backend.is_fullscreen_playing());

        // 循环视频不产生完成事件
        let events = backend.poll_finish_events();
        assert_eq!(events.len(), 0);

        // advance 后视频仍在播放
        backend.advance(16);
        assert!(backend.is_fullscreen_playing());
    }

    #[test]
    fn test_stub_video_layer() {
        let mut backend = StubVideoBackend::new();

        let config = VideoConfig {
            file: "test.ogv".to_string(),
            skippable: true,
            loop_play: false,
            delay_margin_ms: None,
        };

        backend.play_layer("1", &config);
        assert!(backend.is_layer_playing("1"));

        let events = backend.poll_finish_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, Some("1".to_string()));

        backend.advance(16);
        assert!(!backend.is_layer_playing("1"));
    }

    #[test]
    fn test_stub_finish_handler() {
        let mut backend = StubVideoBackend::new();

        let handler = VideoFinishHandler {
            file: Some("script.asb".to_string()),
            label: Some("@finish".to_string()),
            call: false,
            handler: None,
        };

        backend.set_finish_handler(handler);
        assert!(backend.video_state().finish_handler.is_some());

        backend.remove_finish_handler();
        assert!(backend.video_state().finish_handler.is_none());
    }
}
