use art3m1s_core::{Project, host::ResourceProvider, host_files::HostResources};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct MemoryHost {
    files: Mutex<HashMap<String, Vec<u8>>>,
    reads: Mutex<Vec<(String, u64)>>,
}

impl MemoryHost {
    fn with_file(key: &str, bytes: &[u8]) -> Arc<Self> {
        let host = Arc::new(Self::default());
        host.files.lock().unwrap().insert(key.into(), bytes.into());
        host
    }
}

impl ResourceProvider for MemoryHost {
    fn query_size(&self, key: &str) -> Result<Option<u64>, String> {
        Ok(self.files.lock().unwrap().get(key).map(|b| b.len() as u64))
    }

    fn read_range(&self, key: &str, offset: u64, output: &mut [u8]) -> Result<usize, String> {
        self.reads.lock().unwrap().push((key.into(), offset));
        let files = self.files.lock().unwrap();
        let bytes = files.get(key).ok_or_else(|| format!("missing {key}"))?;
        let offset = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        // Exercise real provider short reads, not just an in-memory memcpy.
        let count = (bytes.len() - offset).min(output.len()).min(3);
        output[..count].copy_from_slice(&bytes[offset..offset + count]);
        Ok(count)
    }

    fn write(&self, key: &str, data: &[u8]) -> Result<(), String> {
        if key == "save/denied" {
            return Err("host denied write".into());
        }
        self.files.lock().unwrap().insert(key.into(), data.into());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        self.files.lock().unwrap().remove(key);
        Ok(())
    }
}

#[test]
fn host_provider_preserves_short_reads_empty_files_and_save_results() {
    let host = MemoryHost::with_file("data/value.bin", b"abcdefgh");
    let resources = HostResources::from_provider(host.clone());
    assert_eq!(resources.read_file("DATA\\value.bin").unwrap(), b"abcdefgh");
    assert_eq!(host.reads.lock().unwrap().len(), 3);
    resources.write("save/empty", b"").unwrap();
    assert_eq!(resources.query_size("save/empty").unwrap(), Some(0));
    assert_eq!(resources.read_file("save/empty").unwrap(), b"");
    assert_eq!(resources.query_size("save/absent").unwrap(), None);
    resources
        .set_override("save/value", b"old".to_vec())
        .unwrap();
    resources.write("save/value", b"new").unwrap();
    assert_eq!(resources.read_file("save/value").unwrap(), b"new");
    resources
        .set_override("save/denied", b"unchanged".to_vec())
        .unwrap();
    assert_eq!(
        resources.write("save/denied", b"bad").unwrap_err(),
        "host denied write"
    );
    assert_eq!(resources.read_file("save/denied").unwrap(), b"unchanged");
    resources.delete("save/value").unwrap();
    assert_eq!(resources.query_size("save/value").unwrap(), None);
    let media = resources.open_media_source("data/value.bin").unwrap();
    let mut output = [0; 3];
    assert_eq!(media.read_at(4, &mut output).unwrap(), 3);
    assert_eq!(&output, b"efg");
}

#[test]
fn provider_handles_stay_isolated_and_cannot_fall_back_to_physical_mounts() {
    let resources_a = HostResources::from_provider(MemoryHost::with_file("value", b"A"));
    let resources_b = HostResources::from_provider(MemoryHost::with_file("value", b"B"));
    resources_a
        .set_override("value", b"override".to_vec())
        .unwrap();
    assert_eq!(resources_a.clone().read_file("value").unwrap(), b"override");
    assert_eq!(resources_b.read_file("value").unwrap(), b"B");
    resources_a.clear();
    assert!(resources_a.is_mounted());
    assert_eq!(resources_a.read_file("value").unwrap(), b"A");
    for path in [
        "/etc/passwd",
        "\\root",
        "../value",
        "data/../value",
        "C:\\value",
        "nul\0key",
    ] {
        assert!(resources_a.query_size(path).is_err(), "{path:?}");
        assert!(resources_a.read_file(path).is_err(), "{path:?}");
        assert!(resources_a.write(path, b"bad").is_err(), "{path:?}");
        assert!(resources_a.delete(path).is_err(), "{path:?}");
        assert!(resources_a.set_override(path, vec![]).is_err(), "{path:?}");
    }
    // Rejected before inspecting any filesystem path.
    assert!(resources_a.mount_directory(Path::new(".")).is_err());
    assert!(
        resources_a
            .mount_pfs(Path::new("game.pfs"), "utf-8")
            .is_err()
    );
    assert!(resources_a.set_save_dir(Some(Path::new("save"))).is_err());
    assert!(resources_a.read_file("Cargo.toml").is_err());
}

#[test]
fn interpreter_boot_and_lua_file_access_use_the_explicit_host_provider() {
    let host = MemoryHost::with_file(
        "boot.ast",
        br#"*top
[lua]
local e = __engine
counter = (counter or 0) + 1
payload = e:file("DATA/value.txt")
writer = assert(io.open("save/value.txt", "w"))
writer:write(payload)
writer:close()
assert(io.type(writer) == "closed file")
local reader = assert(io.open("save/value.txt", "rb"))
assert(reader:read(4) == "Host")
assert(reader:seek("set", 0) == 0)
assert(reader:read("*a") == payload)
assert(reader:read() == nil)
reader:close()
local append = assert(io.open("save/value.txt", "a+"))
append:seek("set", 0)
assert(append:write("!") == append)
append:flush()
assert(e:file("save/value.txt") == payload .. "!")
append:close()
local update = assert(io.open("save/numbers.txt", "w+"))
update:write("12.5 first\nsecond\n")
update:seek("set", 0)
assert(update:read("*n") == 12.5)
assert(update:read("*l") == " first")
assert(update:read("*l") == "second")
update:seek("end", 100)
assert(update:read("*n") == nil)
update:close()
local lines = {}
for line in io.lines("save/numbers.txt") do table.insert(lines, line) end
assert(lines[1] == "12.5 first" and lines[2] == "second")
local denied, reason = io.open("save/denied", "w")
assert(denied == nil and reason ~= nil)
assert(io.open("missing.txt", "r") == nil)
assert(io.open("save/bad", "bbww") == nil)
[/lua]
[stop]
"#,
    );
    host.files
        .lock()
        .unwrap()
        .insert("data/value.txt".into(), b"Host bytes".to_vec());
    let resources = HostResources::from_provider(host);
    let project = Project::open_from_data(
        "",
        "[WINDOWS]\nWIDTH=64\nHEIGHT=64\nBOOT=boot.ast\nCHARSET=UTF-8\n",
        "WINDOWS",
    )
    .unwrap();
    let mut interpreter = project.create_interpreter_with_resources(Some(&resources));
    // Match runtime callbacks without opening any directory or installing a global mount.
    struct Callbacks(HostResources);
    impl asb_interpreter::EngineCallbacks for Callbacks {
        fn debug(&self, _: i32, _: &str, _: bool) {}
        fn enqueue_tag(&self, _: String, _: HashMap<String, String>) {}
        fn set_event_handler(&self, _: HashMap<String, String>) {}
        fn get_script_status(&self) -> u8 {
            0
        }
        fn is_key_down(&self, _: u32) -> bool {
            false
        }
        fn is_key_down_edge(&self, _: u32) -> bool {
            false
        }
        fn is_key_up_edge(&self, _: u32) -> bool {
            false
        }
        fn is_decide(&self) -> bool {
            false
        }
        fn is_push(&self, _: u32) -> bool {
            false
        }
        fn get_mouse_point(&self) -> (i32, i32) {
            (0, 0)
        }
        fn get_touch_count(&self) -> u32 {
            0
        }
        fn get_touch_point(&self, _: u32) -> (i32, i32) {
            (0, 0)
        }
        fn is_file_exists(&self, key: &str) -> bool {
            self.0.query_size(key).ok().flatten().is_some()
        }
        fn file_write(&self, key: &str, bytes: &[u8]) -> asb_interpreter::Result<()> {
            self.0
                .write(key, bytes)
                .map_err(|e| asb_interpreter::Error::IoError(std::io::Error::other(e)))
        }
        fn file_operation(&self, _: &str, _: HashMap<String, String>) {}
        fn include(&self, _: &str) {}
        fn override_key(&self, _: u32, _: u32) {}
        fn set_flick_sensitivity(&self, _: f64) {}
        fn get_script_block(&self) -> HashMap<String, String> {
            HashMap::new()
        }
        fn get_script_stack(&self) -> Vec<HashMap<String, String>> {
            Vec::new()
        }
        fn get_script_wait_reason(&self) -> u8 {
            0
        }
    }
    interpreter.set_engine_callbacks(Box::new(Callbacks(resources.clone())));
    project.start_boot(&mut interpreter).unwrap();
    interpreter.run().unwrap();
    assert_eq!(
        interpreter
            .lua()
            .globals()
            .get::<String>("payload")
            .unwrap(),
        "Host bytes"
    );
    assert_eq!(
        interpreter.lua().globals().get::<i32>("counter").unwrap(),
        1
    );
    assert_eq!(
        resources.read_file("save/value.txt").unwrap(),
        b"Host bytes!"
    );
}
