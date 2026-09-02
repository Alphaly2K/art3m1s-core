use eluna::EmoteStaticSprite;
use glam::{Affine2, Vec2};

const MAX_ERROR_PIXELS: f32 = 0.25;

pub(super) fn sprite_vertices(sprite: &EmoteStaticSprite, transform: Affine2) -> Vec<[f32; 4]> {
    let (nx, ny) = sprite.mesh_divisions();
    tessellate(nx as usize, ny as usize, transform, |x, y| {
        let u = x as f32 / nx as f32;
        let v = y as f32 / ny as f32;
        let point = sprite.local_point(u, v);
        // The command affine already supplies the icon pivot and world transform.
        [
            point[0] - sprite.left(),
            point[1] - sprite.top(),
            sprite.uv_left + (sprite.uv_right - sprite.uv_left) * u,
            sprite.uv_top + (sprite.uv_bottom - sprite.uv_top) * v,
        ]
    })
}

fn tessellate(
    nx: usize,
    ny: usize,
    transform: Affine2,
    mut vertex: impl FnMut(usize, usize) -> [f32; 4],
) -> Vec<[f32; 4]> {
    let mut grid = Vec::with_capacity((nx + 1) * (ny + 1));
    for y in 0..=ny {
        for x in 0..=nx {
            grid.push(vertex(x, y));
        }
    }
    // Equal strides dividing both axes keep coarse triangle edges aligned
    // with the authored lattice. Error at the fine vertices then bounds the
    // piecewise-linear surface error throughout every triangle, not just its
    // center. Keep the authored grid when no coarser mesh meets the tolerance.
    let stride = (2..=nx.min(ny))
        .rev()
        .find(|&stride| {
            nx % stride == 0 && ny % stride == 0 && within_error(&grid, nx, ny, stride, transform)
        })
        .unwrap_or(1);
    let mut vertices = Vec::with_capacity((nx / stride) * (ny / stride) * 6);
    for y in (0..ny).step_by(stride) {
        for x in (0..nx).step_by(stride) {
            let tl = grid[y * (nx + 1) + x];
            let bl = grid[(y + stride) * (nx + 1) + x];
            let tr = grid[y * (nx + 1) + x + stride];
            let br = grid[(y + stride) * (nx + 1) + x + stride];
            vertices.extend_from_slice(&[tl, bl, tr, tr, bl, br]);
        }
    }
    vertices
}

fn within_error(
    grid: &[[f32; 4]],
    nx: usize,
    ny: usize,
    stride: usize,
    transform: Affine2,
) -> bool {
    let position = |x, y| {
        let p: [f32; 4] = grid[y * (nx + 1) + x];
        Vec2::new(p[0], p[1])
    };
    for y in 0..=ny {
        for x in 0..=nx {
            if x % stride == 0 && y % stride == 0 {
                continue;
            }
            let x0 = (x / stride * stride).min(nx - stride);
            let y0 = (y / stride * stride).min(ny - stride);
            let u = (x - x0) as f32 / stride as f32;
            let v = (y - y0) as f32 / stride as f32;
            let tl = position(x0, y0);
            let bl = position(x0, y0 + stride);
            let tr = position(x0 + stride, y0);
            let br = position(x0 + stride, y0 + stride);
            let interpolated = if u + v <= 1.0 {
                tl + (tr - tl) * u + (bl - tl) * v
            } else {
                br + (bl - br) * (1.0 - u) + (tr - br) * (1.0 - v)
            };
            let error = transform.transform_vector2(interpolated - position(x, y));
            if !error.is_finite() || error.length_squared() > MAX_ERROR_PIXELS.powi(2) {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affine_sprite_uses_one_quad_and_evaluates_each_point_once() {
        let mut calls = 0;
        let vertices = tessellate(20, 20, Affine2::IDENTITY, |x, y| {
            calls += 1;
            [
                x as f32 * 3.0 + y as f32,
                y as f32 * 2.0,
                x as f32 / 20.0,
                y as f32 / 20.0,
            ]
        });
        assert_eq!(calls, 21 * 21);
        assert_eq!(vertices.len(), 6);
        assert_eq!(vertices[0], [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(vertices[5], [80.0, 40.0, 1.0, 1.0]);
    }

    #[test]
    fn curvature_keeps_detail_and_respects_projected_scale() {
        let vertex = |x: usize, y: usize| {
            let u = x as f32 / 20.0;
            let v = y as f32 / 20.0;
            [u * 100.0, v * 100.0 + 10.0 * u * (1.0 - u), u, v]
        };
        let normal = tessellate(20, 20, Affine2::IDENTITY, vertex);
        let zoomed = tessellate(20, 20, Affine2::from_scale(Vec2::splat(10.0)), vertex);
        assert!(normal.len() > 6 && normal.len() < 20 * 20 * 6);
        assert!(zoomed.len() > normal.len());
    }

    #[test]
    fn triangle_error_is_not_bilinear_quad_error() {
        let vertices = tessellate(20, 20, Affine2::IDENTITY, |x, y| {
            let u = x as f32 / 20.0;
            let v = y as f32 / 20.0;
            [u * 100.0, v * 100.0 + 20.0 * u * v, u, v]
        });
        assert!(vertices.len() > 6);
    }

    #[test]
    fn non_square_grid_preserves_authored_boundary_and_uv() {
        let vertices = tessellate(8, 4, Affine2::IDENTITY, |x, y| {
            [x as f32, y as f32, x as f32 / 8.0, y as f32 / 4.0]
        });
        assert_eq!(vertices.len(), 12);
        assert_eq!(vertices.last(), Some(&[8.0, 4.0, 1.0, 1.0]));
    }
}
