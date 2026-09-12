//! Conversion from the maintained RFVP fork's frame API.
//!
//! The core adapter stays independent by default. Enabling `rfvp-fork` links
//! this crate to the local fork and proves that the real RFVP command types,
//! not only mirrored fixtures, reach the Art3m1s DrawList converter.

use rfvp::host_api as upstream;

use crate::protocol as local;
use crate::{AdaptedFrame, AdapterError, DrawListAdapter};

impl DrawListAdapter {
    pub fn convert_rfvp_frame(
        &self,
        frame: &upstream::RenderFrame,
    ) -> Result<AdaptedFrame, AdapterError> {
        self.convert_frame(&translate_frame(frame))
    }
}

pub fn translate_frame(frame: &upstream::RenderFrame) -> local::RenderFrame {
    local::RenderFrame {
        commands: frame.commands.iter().map(translate_command).collect(),
        hit_proxies: translate_hit_proxy_table(&frame.hit_proxies),
    }
}

fn translate_command(command: &upstream::RenderCommand) -> local::RenderCommand {
    match command {
        upstream::RenderCommand::DrawImage(command) => {
            local::RenderCommand::DrawImage(local::DrawImageCmd {
                texture: local::TextureHandle(command.texture.0),
                src: translate_rect_u16(command.src),
                dst: translate_rect_i16(command.dst),
                color: translate_rgba8(command.color),
                blend: translate_blend(command.blend),
                effect_id: command.effect_id,
                clip: command.clip.map(translate_rect_i16),
                vertices: command.vertices.map(translate_vertex),
            })
        }
        upstream::RenderCommand::DrawGlyph(command) => {
            local::RenderCommand::DrawGlyph(local::DrawGlyphCmd {
                texture: local::TextureHandle(command.texture.0),
                src: translate_rect_u16(command.src),
                dst: translate_rect_i16(command.dst),
                color: translate_rgba8(command.color),
                clip: command.clip.map(translate_rect_i16),
            })
        }
        upstream::RenderCommand::SetClip(rect) => {
            local::RenderCommand::SetClip(translate_rect_i16(*rect))
        }
        upstream::RenderCommand::ClearClip => local::RenderCommand::ClearClip,
    }
}

fn translate_hit_proxy_table(table: &upstream::HitProxyTable) -> local::HitProxyTable {
    local::HitProxyTable {
        proxies: table
            .proxies
            .iter()
            .map(|proxy| local::HitProxy {
                prim_id: local::PrimId(proxy.prim_id.0),
                rect: translate_rect_i16(proxy.rect),
                enabled: proxy.enabled,
                visible: proxy.visible,
                order: proxy.order,
            })
            .collect(),
    }
}

fn translate_vertex(vertex: upstream::Vertex2D) -> local::Vertex2D {
    local::Vertex2D {
        position: vertex.position,
        tex_coord: vertex.tex_coord,
        color: translate_color(vertex.color),
    }
}

fn translate_color(color: upstream::ColorRgba) -> local::ColorRgba {
    local::ColorRgba {
        r: color.r,
        g: color.g,
        b: color.b,
        a: color.a,
    }
}

fn translate_rgba8(color: upstream::Rgba8) -> local::Rgba8 {
    local::Rgba8 {
        r: color.r,
        g: color.g,
        b: color.b,
        a: color.a,
    }
}

fn translate_rect_i16(rect: upstream::RectI16) -> local::RectI16 {
    local::RectI16 {
        x: rect.x,
        y: rect.y,
        w: rect.w,
        h: rect.h,
    }
}

fn translate_rect_u16(rect: upstream::RectU16) -> local::RectU16 {
    local::RectU16 {
        x: rect.x,
        y: rect.y,
        w: rect.w,
        h: rect.h,
    }
}

fn translate_blend(blend: upstream::CommandBlendMode) -> local::CommandBlendMode {
    match blend {
        upstream::CommandBlendMode::Normal => local::CommandBlendMode::Normal,
        upstream::CommandBlendMode::Add => local::CommandBlendMode::Add,
        upstream::CommandBlendMode::Sub => local::CommandBlendMode::Sub,
        upstream::CommandBlendMode::Mul => local::CommandBlendMode::Mul,
    }
}

#[cfg(test)]
mod tests {
    use art3m1s_render::{BlendMode, TextureId, TextureInfo};

    use super::*;
    use crate::TextureBindings;

    fn vertex(x: f32, y: f32, u: f32, v: f32) -> upstream::Vertex2D {
        upstream::Vertex2D {
            position: [x, y],
            tex_coord: [u, v],
            color: upstream::ColorRgba {
                r: 0.5,
                g: 0.25,
                b: 0.125,
                a: 0.75,
            },
        }
    }

    #[test]
    fn real_rfvp_frame_reaches_art3m1s_draw_list() {
        let upstream_frame = upstream::RenderFrame {
            commands: vec![
                upstream::RenderCommand::SetClip(upstream::RectI16 {
                    x: 4,
                    y: 5,
                    w: 100,
                    h: 80,
                }),
                upstream::RenderCommand::DrawImage(upstream::DrawImageCmd {
                    texture: upstream::TextureHandle(7),
                    src: upstream::RectU16::default(),
                    dst: upstream::RectI16 {
                        x: 10,
                        y: 10,
                        w: 40,
                        h: 40,
                    },
                    color: upstream::Rgba8 {
                        r: 255,
                        g: 255,
                        b: 255,
                        a: 255,
                    },
                    blend: upstream::CommandBlendMode::Sub,
                    effect_id: 0,
                    clip: None,
                    vertices: [
                        vertex(10.0, 50.0, 0.0, 1.0),
                        vertex(10.0, 10.0, 0.0, 0.0),
                        vertex(50.0, 50.0, 1.0, 1.0),
                        vertex(50.0, 10.0, 1.0, 0.0),
                    ],
                }),
            ],
            hit_proxies: upstream::HitProxyTable {
                proxies: vec![upstream::HitProxy {
                    prim_id: upstream::PrimId(42),
                    rect: upstream::RectI16 {
                        x: 1,
                        y: 2,
                        w: 3,
                        h: 4,
                    },
                    enabled: true,
                    visible: true,
                    order: 9,
                }],
            },
        };

        let mut bindings = TextureBindings::new();
        bindings.insert(
            local::TextureHandle(7),
            TextureId(70),
            TextureInfo {
                width: 64,
                height: 32,
            },
        );
        let adapter = DrawListAdapter::with_bindings(bindings);

        let adapted = adapter.convert_rfvp_frame(&upstream_frame).unwrap();
        let command = &adapted.draw_list.commands[0];

        assert_eq!(command.texture, TextureId(70));
        assert_eq!(command.blend, BlendMode::NativeReverseSubtract);
        assert_eq!(command.clip_bounds, Some([4.0, 5.0, 100.0, 80.0]));
        assert_eq!(adapted.hit_proxies.proxies[0].prim_id, local::PrimId(42));
    }
}
