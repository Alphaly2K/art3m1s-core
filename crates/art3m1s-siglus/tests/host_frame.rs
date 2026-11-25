#![cfg(feature = "host-audio")]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use art3m1s_siglus::siglus_scene_vm::{
    formats::scene_pck::ScenePckDecodeOptions, host::SiglusHostConfig, runtime::constants::cd,
};
use art3m1s_siglus::{SiglusAdapter, SiglusBootSource, SiglusGameexe, SiglusResourceSource};

struct BootSource;

impl SiglusBootSource for BootSource {
    fn read_gameexe(&self) -> anyhow::Result<Option<SiglusGameexe>> {
        Ok(Some(SiglusGameexe::Ini(
            b"#SCREEN_SIZE = 320, 240\n".to_vec(),
        )))
    }

    fn initial_append(&self) -> anyhow::Result<(String, String)> {
        Ok((String::new(), String::new()))
    }

    fn load_scene_pack(&self, _append: &str) -> anyhow::Result<(Vec<u8>, ScenePckDecodeOptions)> {
        const PACK_HEADER: i32 = 23 * 4;
        const SCENE_HEADER: i32 = 33 * 4;
        let name: Vec<u16> = "_menu".encode_utf16().collect();
        let name_list = PACK_HEADER + 8;
        let data_index = name_list + name.len() as i32 * 2;
        let data_list = data_index + 8;
        let mut pack = [0i32; 23];
        pack[0] = PACK_HEADER;
        pack[13..21].copy_from_slice(&[PACK_HEADER, 1, name_list, 1, data_index, 1, data_list, 1]);
        let mut scene = [0i32; 33];
        scene[..3].copy_from_slice(&[SCENE_HEADER, SCENE_HEADER + 4, 1]);
        scene[9..11].copy_from_slice(&[SCENE_HEADER, 1]);

        let mut bytes: Vec<u8> = pack.into_iter().flat_map(i32::to_le_bytes).collect();
        bytes.extend(
            [0, name.len() as i32]
                .into_iter()
                .flat_map(i32::to_le_bytes),
        );
        bytes.extend(name.into_iter().flat_map(u16::to_le_bytes));
        bytes.extend([0, SCENE_HEADER + 5].into_iter().flat_map(i32::to_le_bytes));
        bytes.extend(scene.into_iter().flat_map(i32::to_le_bytes));
        bytes.extend(0i32.to_le_bytes());
        bytes.push(cd::EOF);
        Ok((bytes, ScenePckDecodeOptions::default()))
    }
}

#[test]
fn steps_without_a_gpu_and_isolates_host_time_between_sessions() {
    fn open() -> SiglusAdapter {
        let mut config = SiglusHostConfig::new(PathBuf::from("siglus-memory-project"));
        config.scene_id = Some(0);
        SiglusAdapter::open_with_boot_source(config, Arc::new(BootSource)).unwrap()
    }
    let mut a = open();
    let mut b = open();
    for session in [&mut a, &mut b] {
        let frame = session.step(0).unwrap().frame.unwrap();
        assert_eq!(frame.size, (320, 240));
        session.with_host(|host| host.vm_mut().ctx.wait.wait_mwnd_animation(25));
    }
    a.step(24).unwrap();
    assert!(a.with_host(|host| host.vm_mut().ctx.wait.mwnd_animation_waiting()));
    a.step(1).unwrap();
    assert!(!a.with_host(|host| host.vm_mut().is_blocked()));
    assert!(b.with_host(|host| host.vm_mut().is_blocked()));
    b.step(25).unwrap();
    assert!(!b.with_host(|host| host.vm_mut().is_blocked()));
}

struct MemoryProject {
    bmp: [u8; 58],
    append: String,
    selection: String,
}

impl MemoryProject {
    fn new(rgb: [u8; 3], append: &str) -> Self {
        let mut bmp = [0u8; 58];
        bmp[..2].copy_from_slice(b"BM");
        for (start, value) in [(2, 58u32), (10, 54), (14, 40), (18, 1), (22, 1), (34, 4)] {
            bmp[start..start + 4].copy_from_slice(&value.to_le_bytes());
        }
        bmp[26] = 1;
        bmp[28] = 24;
        bmp[54..57].copy_from_slice(&[rgb[2], rgb[1], rgb[0]]);
        Self {
            bmp,
            append: append.into(),
            selection: format!("{append}\tChapter\n"),
        }
    }

    fn file(&self, key: &str) -> Option<&[u8]> {
        if key.eq_ignore_ascii_case("Select.ini") {
            Some(self.selection.as_bytes())
        } else if key.eq_ignore_ascii_case(&format!("{}/g00/color.bmp", self.append)) {
            Some(&self.bmp)
        } else {
            None
        }
    }
}

impl SiglusResourceSource for MemoryProject {
    fn query_size(&self, key: &str) -> anyhow::Result<Option<u64>> {
        Ok(self.file(key).map(|bytes| bytes.len() as u64))
    }

    fn read_range(&self, key: &str, offset: u64, output: &mut [u8]) -> anyhow::Result<usize> {
        let bytes = self.file(key).expect("known resource");
        let data = &bytes[offset as usize..];
        let count = data.len().min(output.len()).min(5);
        output[..count].copy_from_slice(&data[..count]);
        Ok(count)
    }
}

#[test]
fn loads_named_images_from_the_active_host_project() {
    let root = PathBuf::from("siglus-memory-project");
    let open = |rgb, append| {
        let mut config = SiglusHostConfig::new(root.clone());
        config.scene_id = Some(0);
        SiglusAdapter::open_with_project_files(
            config,
            Arc::new(BootSource),
            Arc::new(MemoryProject::new(rgb, append)),
        )
        .unwrap()
    };
    let mut a = open([0x12, 0x34, 0x56], "first");
    let mut b = open([0x78, 0x90, 0xab], "second");
    for (session, expected) in [
        (&mut a, [0x12, 0x34, 0x56, 0xff]),
        (&mut b, [0x78, 0x90, 0xab, 0xff]),
    ] {
        session.with_host(|host| {
            let images = &mut host.vm_mut().ctx.images;
            let image = images.load_bg("COLOR").unwrap();
            assert_eq!(images.get(&image).unwrap().rgba, expected);
            assert!(
                art3m1s_siglus::siglus_scene_vm::resource::read_file_bytes(
                    &root.join("../g00/color.bmp")
                )
                .is_err()
            );
            assert!(images.load_bg("missing").is_err());
        });
    }
    assert!(
        art3m1s_siglus::siglus_scene_vm::resource::resolve_game_file(Path::new(
            "siglus-memory-project/g00/color.bmp"
        ))
        .unwrap()
        .is_none()
    );
}
