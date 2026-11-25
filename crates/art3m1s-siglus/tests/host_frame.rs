#![cfg(feature = "host-audio")]

use std::path::PathBuf;
use std::sync::Arc;

use art3m1s_siglus::siglus_scene_vm::{
    formats::scene_pck::ScenePckDecodeOptions, host::SiglusHostConfig, runtime::constants::cd,
};
use art3m1s_siglus::{SiglusAdapter, SiglusBootSource, SiglusGameexe};

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
