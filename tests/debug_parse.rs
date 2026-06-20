use art3m1s_core::Project;
use std::path::Path;

#[test]
fn debug_parse() {
    let root = Path::new("/Users/alphaly/lfpm/hamidashi");
    let project = Project::open(root, "WINDOWS").unwrap();
    let mut it = project.create_interpreter();

    project.start_boot(&mut it).unwrap();

    if let Some(script) = it.get_script("system/first.iet") {
        eprintln!("=== first.iet 指令 ===");
        for (i, inst) in script.instructions.iter().enumerate() {
            eprintln!("{i}: tag={} params={:?}", inst.tag, inst.params);
            if i > 25 {
                break;
            }
        }
    }
}
