//! 切出指定图集 icon 存 PNG（调试用）。
use art3m1s_emote::EmoteModel;
use std::env;

fn main() {
    let path = env::args().nth(1).expect("model.psb");
    let model = EmoteModel::open(&path).expect("open");
    // (texture_id, name, x, y, w, h)
    let crops: &[(&str, &str, u32, u32, u32, u32)] = &[
        ("tex#005", "shirome_open_0074", 1697, 1767, 102, 78),
        ("tex#005", "shirome_half_0075", 1697, 1845, 102, 78),
        ("tex#005", "mabuta_open_0038", 1441, 1364, 172, 157),
        ("tex#005", "mabuta_half_0039", 1441, 1521, 172, 157),
        ("tex#005", "mabuta_closed_0040", 1441, 1678, 172, 157),
        ("tex#005", "iris_0062", 1957, 1313, 81, 102),
    ];
    for (tex_id, name, x, y, w, h) in crops {
        let texture = model.atlas().texture(tex_id).expect("tex");
        let rgba = model
            .atlas()
            .decode_texture_rgba8(model.document(), tex_id)
            .expect("decode");
        let tw = texture.width;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in *y..y + h {
            let start = ((row * tw + x) * 4) as usize;
            out.extend_from_slice(&rgba[start..start + (*w as usize) * 4]);
        }
        // 打 alpha 覆盖率
        let cover = out.chunks(4).filter(|p| p[3] > 8).count();
        eprintln!("{name}: alpha>8 {}/{} ({}%)", cover, (w * h), cover * 100 / (*w as usize * *h as usize));
        image::save_buffer(
            format!("/tmp/icon_{name}.png"),
            &out,
            *w,
            *h,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
}
