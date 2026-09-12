use std::fmt;
use std::sync::Arc;

use art3m1s_render::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawList, DrawMesh, TextureInfo,
};
use glam::{Affine2, Vec2};

use crate::protocol::{
    ColorRgba, CommandBlendMode, DrawGlyphCmd, DrawImageCmd, DrawSolidCmd, HitProxyTable, RectI16,
    RectU16, RenderCommand, RenderFrame, Vertex2D,
};
use crate::textures::{TextureBinding, TextureBindings};

const EPSILON: f32 = 1.0e-5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterError {
    UnknownTexture(u32),
    MissingSolidTexture,
    UnsupportedEffect(u16),
    NonUniformVertexColor,
    InvalidClip(RectI16),
    InvalidRect(RectI16),
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTexture(id) => write!(f, "RFVP texture handle {id} is not bound"),
            Self::MissingSolidTexture => {
                write!(f, "RFVP solid command requires a bound 1x1 white texture")
            }
            Self::UnsupportedEffect(id) => {
                write!(
                    f,
                    "RFVP effect id {id} is not supported by the draw-list ABI"
                )
            }
            Self::NonUniformVertexColor => {
                write!(f, "RFVP command has per-vertex colors")
            }
            Self::InvalidClip(rect) => write!(f, "RFVP clip rect is invalid: {rect:?}"),
            Self::InvalidRect(rect) => write!(f, "RFVP draw rect is invalid: {rect:?}"),
        }
    }
}

impl std::error::Error for AdapterError {}

#[derive(Debug, Clone, PartialEq)]
pub struct AdaptedFrame {
    pub draw_list: DrawList,
    pub hit_proxies: HitProxyTable,
}

#[derive(Debug, Clone, Default)]
pub struct DrawListAdapter {
    bindings: TextureBindings,
}

impl DrawListAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_bindings(bindings: TextureBindings) -> Self {
        Self { bindings }
    }

    pub fn bindings(&self) -> &TextureBindings {
        &self.bindings
    }

    pub fn bindings_mut(&mut self) -> &mut TextureBindings {
        &mut self.bindings
    }

    pub fn convert_frame(&self, frame: &RenderFrame) -> Result<AdaptedFrame, AdapterError> {
        let draw_list = self.convert_commands(&frame.commands)?;
        Ok(AdaptedFrame {
            draw_list,
            hit_proxies: frame.hit_proxies.clone(),
        })
    }

    pub fn convert_commands(&self, commands: &[RenderCommand]) -> Result<DrawList, AdapterError> {
        let mut draw_list = DrawList::new();
        let mut clip = None;

        for command in commands {
            match command {
                RenderCommand::SetClip(rect) => {
                    validate_clip(*rect)?;
                    clip = Some(*rect);
                }
                RenderCommand::ClearClip => clip = None,
                RenderCommand::DrawImage(command) => {
                    draw_list.push(convert_image(
                        command,
                        command.clip.or(clip),
                        &self.bindings,
                    )?);
                }
                RenderCommand::DrawGlyph(command) => {
                    draw_list.push(convert_glyph(
                        command,
                        command.clip.or(clip),
                        &self.bindings,
                    )?);
                }
                RenderCommand::DrawSolid(command) => {
                    draw_list.push(convert_solid(
                        command,
                        command.clip.or(clip),
                        &self.bindings,
                    )?);
                }
            }
        }

        Ok(draw_list)
    }
}

fn convert_image(
    command: &DrawImageCmd,
    clip: Option<RectI16>,
    bindings: &TextureBindings,
) -> Result<DrawCommand, AdapterError> {
    if command.effect_id != 0 {
        return Err(AdapterError::UnsupportedEffect(command.effect_id));
    }

    let binding = resolve(bindings, command.texture.0)?;
    let color = uniform_vertex_color(&command.vertices)?;
    let clip_bounds = clip_bounds(clip)?;
    let (transform, clip_rect, mesh) = sprite_geometry(&command.vertices, binding.info);

    Ok(DrawCommand {
        texture: binding.texture,
        size: binding.info,
        transform,
        opacity: color.a,
        blend: blend_mode(command.blend),
        color: ColorFilter {
            multiply: [color.r, color.g, color.b],
            ..ColorFilter::default()
        },
        clip: clip_rect,
        clip_bounds,
        shader: None,
        mesh,
        stencil: None,
        native_emote: None,
    })
}

fn convert_glyph(
    command: &DrawGlyphCmd,
    clip: Option<RectI16>,
    bindings: &TextureBindings,
) -> Result<DrawCommand, AdapterError> {
    validate_rect(command.dst)?;
    let binding = resolve(bindings, command.texture.0)?;
    let color = command.color.to_color_rgba();
    let (uv_offset, uv_scale) = source_uv(command.src, binding.info);

    Ok(DrawCommand {
        texture: binding.texture,
        size: binding.info,
        transform: Affine2::from_translation(Vec2::new(command.dst.x as f32, command.dst.y as f32)),
        opacity: color.a,
        blend: BlendMode::Alpha,
        color: ColorFilter {
            multiply: [color.r, color.g, color.b],
            ..ColorFilter::default()
        },
        clip: ClipRect {
            uv_offset,
            uv_scale,
            quad_size: [command.dst.w as f32, command.dst.h as f32],
        },
        clip_bounds: clip_bounds(clip)?,
        shader: None,
        mesh: None,
        stencil: None,
        native_emote: None,
    })
}

fn convert_solid(
    command: &DrawSolidCmd,
    clip: Option<RectI16>,
    bindings: &TextureBindings,
) -> Result<DrawCommand, AdapterError> {
    validate_rect(command.rect)?;
    let binding = bindings.solid().ok_or(AdapterError::MissingSolidTexture)?;

    Ok(DrawCommand {
        texture: binding.texture,
        size: binding.info,
        transform: Affine2::from_translation(Vec2::new(
            command.rect.x as f32,
            command.rect.y as f32,
        )),
        opacity: command.color.a,
        blend: blend_mode(command.blend),
        color: ColorFilter {
            multiply: [command.color.r, command.color.g, command.color.b],
            ..ColorFilter::default()
        },
        clip: ClipRect {
            uv_offset: [0.0, 0.0],
            uv_scale: [1.0, 1.0],
            quad_size: [command.rect.w as f32, command.rect.h as f32],
        },
        clip_bounds: clip_bounds(clip)?,
        shader: None,
        mesh: None,
        stencil: None,
        native_emote: None,
    })
}

fn sprite_geometry(
    vertices: &[Vertex2D; 4],
    info: TextureInfo,
) -> (Affine2, ClipRect, Option<DrawMesh>) {
    let [bl, tl, br, tr] = vertices;

    if is_axis_aligned_rect(bl, tl, br, tr) {
        return (
            Affine2::from_translation(Vec2::new(tl.position[0], tl.position[1])),
            ClipRect {
                uv_offset: tl.tex_coord,
                uv_scale: [
                    br.tex_coord[0] - tl.tex_coord[0],
                    bl.tex_coord[1] - tl.tex_coord[1],
                ],
                quad_size: [
                    br.position[0] - tl.position[0],
                    bl.position[1] - tl.position[1],
                ],
            },
            None,
        );
    }

    // RFVP emits BL, TL, BR, TR. Split on TL-BR so the triangulation matches
    // the core quad path and does not change interpolation on warped quads.
    let mesh_vertices = Arc::<[[f32; 4]]>::from(vec![
        mesh_vertex(*tl),
        mesh_vertex(*tr),
        mesh_vertex(*br),
        mesh_vertex(*tl),
        mesh_vertex(*br),
        mesh_vertex(*bl),
    ]);

    (
        Affine2::IDENTITY,
        ClipRect {
            uv_offset: [0.0, 0.0],
            uv_scale: [1.0, 1.0],
            quad_size: [info.width as f32, info.height as f32],
        },
        Some(DrawMesh {
            vertices: mesh_vertices,
        }),
    )
}

fn mesh_vertex(vertex: Vertex2D) -> [f32; 4] {
    [
        vertex.position[0],
        vertex.position[1],
        vertex.tex_coord[0],
        vertex.tex_coord[1],
    ]
}

fn is_axis_aligned_rect(bl: &Vertex2D, tl: &Vertex2D, br: &Vertex2D, tr: &Vertex2D) -> bool {
    approx_eq(bl.position[0], tl.position[0])
        && approx_eq(br.position[0], tr.position[0])
        && approx_eq(tl.position[1], tr.position[1])
        && approx_eq(bl.position[1], br.position[1])
        && approx_eq(bl.tex_coord[0], tl.tex_coord[0])
        && approx_eq(br.tex_coord[0], tr.tex_coord[0])
        && approx_eq(tl.tex_coord[1], tr.tex_coord[1])
        && approx_eq(bl.tex_coord[1], br.tex_coord[1])
        && br.position[0] > tl.position[0]
        && bl.position[1] > tl.position[1]
}

fn approx_eq(left: f32, right: f32) -> bool {
    (left - right).abs() <= EPSILON
}

fn uniform_vertex_color(vertices: &[Vertex2D; 4]) -> Result<ColorRgba, AdapterError> {
    let color = vertices[0].color;
    if vertices.iter().skip(1).all(|vertex| vertex.color == color) {
        Ok(color)
    } else {
        Err(AdapterError::NonUniformVertexColor)
    }
}

fn source_uv(src: RectU16, info: TextureInfo) -> ([f32; 2], [f32; 2]) {
    if src.w == 0 || src.h == 0 {
        return ([0.0, 0.0], [1.0, 1.0]);
    }
    let width = info.width.max(1) as f32;
    let height = info.height.max(1) as f32;
    (
        [src.x as f32 / width, src.y as f32 / height],
        [src.w as f32 / width, src.h as f32 / height],
    )
}

fn resolve(bindings: &TextureBindings, handle: u32) -> Result<TextureBinding, AdapterError> {
    bindings
        .get(crate::protocol::TextureHandle(handle))
        .ok_or(AdapterError::UnknownTexture(handle))
}

fn blend_mode(mode: CommandBlendMode) -> BlendMode {
    match mode {
        CommandBlendMode::Normal => BlendMode::Alpha,
        CommandBlendMode::Add => BlendMode::Add,
        CommandBlendMode::Sub => BlendMode::NativeReverseSubtract,
        CommandBlendMode::Mul => BlendMode::Multiply,
    }
}

fn validate_clip(rect: RectI16) -> Result<(), AdapterError> {
    if rect.w < 0 || rect.h < 0 {
        Err(AdapterError::InvalidClip(rect))
    } else {
        Ok(())
    }
}

fn validate_rect(rect: RectI16) -> Result<(), AdapterError> {
    if rect.w < 0 || rect.h < 0 {
        Err(AdapterError::InvalidRect(rect))
    } else {
        Ok(())
    }
}

fn clip_bounds(rect: Option<RectI16>) -> Result<Option<[f32; 4]>, AdapterError> {
    let Some(rect) = rect else {
        return Ok(None);
    };
    validate_clip(rect)?;
    Ok(Some([
        rect.x as f32,
        rect.y as f32,
        rect.w as f32,
        rect.h as f32,
    ]))
}

#[cfg(test)]
mod tests {
    use art3m1s_render::{BlendMode, TextureId, TextureInfo};

    use super::*;
    use crate::protocol::{HitProxy, PrimId, RectU16, Rgba8, TextureHandle};

    fn color(r: f32, g: f32, b: f32, a: f32) -> ColorRgba {
        ColorRgba { r, g, b, a }
    }

    fn rgba(r: u8, g: u8, b: u8, a: u8) -> Rgba8 {
        Rgba8 { r, g, b, a }
    }

    fn vertex(x: f32, y: f32, u: f32, v: f32, color: ColorRgba) -> Vertex2D {
        Vertex2D {
            position: [x, y],
            tex_coord: [u, v],
            color,
        }
    }

    fn axis_vertices(color: ColorRgba) -> [Vertex2D; 4] {
        [
            vertex(10.0, 50.0, 0.0, 1.0, color),
            vertex(10.0, 10.0, 0.0, 0.0, color),
            vertex(50.0, 50.0, 1.0, 1.0, color),
            vertex(50.0, 10.0, 1.0, 0.0, color),
        ]
    }

    fn image(
        texture: u32,
        vertices: [Vertex2D; 4],
        color: Rgba8,
        blend: CommandBlendMode,
    ) -> RenderCommand {
        RenderCommand::DrawImage(DrawImageCmd {
            texture: TextureHandle(texture),
            src: RectU16::default(),
            dst: RectI16 {
                x: 10,
                y: 10,
                w: 40,
                h: 40,
            },
            color,
            blend,
            effect_id: 0,
            clip: None,
            vertices,
        })
    }

    fn bindings() -> TextureBindings {
        let mut bindings = TextureBindings::new();
        bindings.insert(
            TextureHandle(7),
            TextureId(70),
            TextureInfo {
                width: 64,
                height: 32,
            },
        );
        bindings.set_solid(
            TextureId(99),
            TextureInfo {
                width: 1,
                height: 1,
            },
        );
        bindings
    }

    #[test]
    fn axis_aligned_image_uses_quad_and_preserves_color() {
        let tint = color(0.5, 0.25, 0.125, 0.75);
        let adapter = DrawListAdapter::with_bindings(bindings());
        let frame = RenderFrame {
            commands: vec![image(
                7,
                axis_vertices(tint),
                rgba(128, 64, 32, 192),
                CommandBlendMode::Sub,
            )],
            hit_proxies: HitProxyTable::default(),
        };

        let adapted = adapter.convert_frame(&frame).unwrap();
        let command = &adapted.draw_list.commands[0];

        assert_eq!(command.texture, TextureId(70));
        assert_eq!(
            command.size,
            TextureInfo {
                width: 64,
                height: 32
            }
        );
        assert_eq!(command.blend, BlendMode::NativeReverseSubtract);
        assert!((command.opacity - tint.a).abs() < EPSILON);
        assert_eq!(command.color.multiply, [tint.r, tint.g, tint.b]);
        assert_eq!(command.transform.translation, Vec2::new(10.0, 10.0));
        assert_eq!(command.clip.uv_offset, [0.0, 0.0]);
        assert_eq!(command.clip.uv_scale, [1.0, 1.0]);
        assert_eq!(command.clip.quad_size, [40.0, 40.0]);
        assert!(command.mesh.is_none());
    }

    #[test]
    fn rotated_image_uses_mesh_with_core_quad_triangulation() {
        let tint = color(1.0, 1.0, 1.0, 1.0);
        let vertices = [
            vertex(0.0, 10.0, 0.0, 1.0, tint),
            vertex(10.0, 0.0, 0.0, 0.0, tint),
            vertex(20.0, 10.0, 1.0, 1.0, tint),
            vertex(10.0, 20.0, 1.0, 0.0, tint),
        ];
        let adapter = DrawListAdapter::with_bindings(bindings());
        let frame = RenderFrame {
            commands: vec![image(
                7,
                vertices,
                rgba(255, 255, 255, 255),
                CommandBlendMode::Normal,
            )],
            hit_proxies: HitProxyTable::default(),
        };

        let adapted = adapter.convert_frame(&frame).unwrap();
        let command = &adapted.draw_list.commands[0];
        let mesh = command.mesh.as_ref().expect("mesh path");

        assert_eq!(command.transform, Affine2::IDENTITY);
        assert_eq!(mesh.vertices.len(), 6);
        assert_eq!(mesh.vertices[0], [10.0, 0.0, 0.0, 0.0]);
        assert_eq!(mesh.vertices[1], [10.0, 20.0, 1.0, 0.0]);
        assert_eq!(mesh.vertices[2], [20.0, 10.0, 1.0, 1.0]);
        assert_eq!(mesh.vertices[3], [10.0, 0.0, 0.0, 0.0]);
        assert_eq!(mesh.vertices[4], [20.0, 10.0, 1.0, 1.0]);
        assert_eq!(mesh.vertices[5], [0.0, 10.0, 0.0, 1.0]);
    }

    #[test]
    fn clip_state_applies_until_clear_and_can_be_overridden() {
        let tint = color(1.0, 1.0, 1.0, 1.0);
        let mut glyph = DrawGlyphCmd {
            texture: TextureHandle(7),
            src: RectU16 {
                x: 16,
                y: 8,
                w: 32,
                h: 16,
            },
            dst: RectI16 {
                x: 2,
                y: 3,
                w: 32,
                h: 16,
            },
            color: rgba(255, 255, 255, 255),
            clip: None,
        };
        let frame = RenderFrame {
            commands: vec![
                RenderCommand::SetClip(RectI16 {
                    x: 5,
                    y: 6,
                    w: 100,
                    h: 80,
                }),
                image(
                    7,
                    axis_vertices(tint),
                    rgba(255, 255, 255, 255),
                    CommandBlendMode::Normal,
                ),
                RenderCommand::ClearClip,
                RenderCommand::DrawGlyph(glyph),
            ],
            hit_proxies: HitProxyTable::default(),
        };

        let adapter = DrawListAdapter::with_bindings(bindings());
        let adapted = adapter.convert_frame(&frame).unwrap();

        assert_eq!(
            adapted.draw_list.commands[0].clip_bounds,
            Some([5.0, 6.0, 100.0, 80.0])
        );
        assert_eq!(adapted.draw_list.commands[1].clip_bounds, None);
        assert_eq!(adapted.draw_list.commands[1].clip.uv_offset, [0.25, 0.25]);
        assert_eq!(adapted.draw_list.commands[1].clip.uv_scale, [0.5, 0.5]);

        glyph.clip = Some(RectI16 {
            x: 7,
            y: 8,
            w: 9,
            h: 10,
        });
        let command = adapter
            .convert_commands(&[
                RenderCommand::SetClip(RectI16 {
                    x: 1,
                    y: 2,
                    w: 3,
                    h: 4,
                }),
                RenderCommand::DrawGlyph(glyph),
            ])
            .unwrap();
        assert_eq!(command.commands[0].clip_bounds, Some([7.0, 8.0, 9.0, 10.0]));
    }

    #[test]
    fn solid_uses_white_texture_and_maps_blend() {
        let frame = RenderFrame {
            commands: vec![RenderCommand::DrawSolid(DrawSolidCmd {
                rect: RectI16 {
                    x: 4,
                    y: 5,
                    w: 30,
                    h: 20,
                },
                color: color(0.25, 0.5, 0.75, 0.5),
                blend: CommandBlendMode::Add,
                clip: None,
            })],
            hit_proxies: HitProxyTable::default(),
        };
        let adapter = DrawListAdapter::with_bindings(bindings());

        let adapted = adapter.convert_frame(&frame).unwrap();
        let command = &adapted.draw_list.commands[0];

        assert_eq!(command.texture, TextureId(99));
        assert_eq!(command.transform.translation, Vec2::new(4.0, 5.0));
        assert_eq!(command.clip.quad_size, [30.0, 20.0]);
        assert_eq!(command.blend, BlendMode::Add);
        assert_eq!(command.opacity, 0.5);
        assert_eq!(command.color.multiply, [0.25, 0.5, 0.75]);
    }

    #[test]
    fn hit_proxies_are_preserved_for_the_host() {
        let frame = RenderFrame {
            commands: Vec::new(),
            hit_proxies: HitProxyTable {
                proxies: vec![HitProxy {
                    prim_id: PrimId(12),
                    rect: RectI16 {
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
        let adapter = DrawListAdapter::with_bindings(bindings());

        let adapted = adapter.convert_frame(&frame).unwrap();

        assert_eq!(adapted.hit_proxies, frame.hit_proxies);
    }

    #[test]
    fn unsupported_or_invalid_commands_fail_explicitly() {
        let tint = color(1.0, 1.0, 1.0, 1.0);
        let adapter = DrawListAdapter::with_bindings(bindings());

        let mut draw = match image(
            7,
            axis_vertices(tint),
            rgba(255, 255, 255, 255),
            CommandBlendMode::Normal,
        ) {
            RenderCommand::DrawImage(command) => command,
            _ => unreachable!(),
        };
        draw.effect_id = 3;
        assert_eq!(
            adapter.convert_commands(&[RenderCommand::DrawImage(draw)]),
            Err(AdapterError::UnsupportedEffect(3))
        );

        let mut unsupported_color = axis_vertices(tint);
        unsupported_color[3].color.a = 0.5;
        assert_eq!(
            adapter.convert_commands(&[image(
                7,
                unsupported_color,
                rgba(255, 255, 255, 255),
                CommandBlendMode::Normal,
            )]),
            Err(AdapterError::NonUniformVertexColor)
        );

        assert_eq!(
            adapter.convert_commands(&[image(
                404,
                axis_vertices(tint),
                rgba(255, 255, 255, 255),
                CommandBlendMode::Normal,
            )]),
            Err(AdapterError::UnknownTexture(404))
        );
    }
}
