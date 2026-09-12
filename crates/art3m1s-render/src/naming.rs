//! Shared reserved resource naming used by host video import and backends.

/// Host-decoded layer video textures live in a reserved resource namespace.
pub const VIDEO_LAYER_TEXTURE_PREFIX: &str = "__video_layer__:";

pub fn video_layer_texture_name(id: &str) -> String {
    format!("{VIDEO_LAYER_TEXTURE_PREFIX}{id}")
}

pub fn is_video_layer_texture_name(name: &str) -> bool {
    name.starts_with(VIDEO_LAYER_TEXTURE_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_texture_name_uses_reserved_namespace() {
        let name = video_layer_texture_name("mw.movie");
        assert_eq!(name, "__video_layer__:mw.movie");
        assert!(is_video_layer_texture_name(&name));
        assert!(!is_video_layer_texture_name("movie/opening"));
    }
}
