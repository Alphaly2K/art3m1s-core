//! 验证 syssave/sysload 的核心往返逻辑（不经宿主 FFI，纯解释器层）。
//!
//! 复现报告的两个症状：
//!   1) config 改了后重启没恢复——怀疑 g.config 在 boot 前没被种回，或 fload_pluto
//!      读不回来。
//!   2) 编号存档没建文件——单独排查。
//!
//! 这里只验证「把 g.config 当作 sysload 那样种进变量域后，脚本自己的
//! fload_pluto('g.config') 能否读回该表」。这是 system_dataloading→load_config
//! 的实际路径。

use art3m1s_core::Project;
use asb_interpreter::Value;
use std::path::Path;

#[test]
fn syssave_config_roundtrips_through_fload_pluto() {
    let root = Path::new("/Users/alphaly/lfpm/loli/root");
    if !root.join("system.ini").exists() {
        eprintln!("跳过：loli 项目不在 {root:?}");
        return;
    }
    let project = Project::open(root, "WINDOWS").unwrap();
    let mut it = project.create_interpreter();

    // boot 注册 pluto / e:include 等基础设施
    project.start_boot(&mut it).unwrap();
    // 跑几步让 init.lua / fileio.lua 被 include、pluto 桩注入
    for _ in 0..40 {
        match it.run() {
            Ok(asb_interpreter::ExecutionResult::Wait(_)) => it.advance_line(),
            Ok(_) => break,
            Err(e) => {
                eprintln!("boot 推进出错（可接受，仅需基础设施就绪）: {e:?}");
                break;
            }
        }
    }

    // 模拟 sysload：把 saveg.dat 里的 g.config 种回变量域。
    // 用脚本真实写过的一份 config JSON 的最小子集。
    let config_json = r#"{"bgm":60,"se":100,"voice":100,"aspeed":50,"mspeed":80,"language":"ja"}"#;
    it.set_variable("g.config", Value::String(config_json.to_string()));

    // 现在跑脚本自己的 load_config()（= conf = fload_pluto('g.config')），
    // 然后把 conf.bgm 暴露到一个普通全局，读回来断言。
    let probe = r#"
        load_config()
        __probe_bgm = tostring(conf and conf.bgm or "nil")
        __probe_lang = tostring(conf and conf.language or "nil")
        __probe_type = type(conf)
    "#;
    it.lua()
        .load(probe)
        .exec()
        .expect("执行 load_config 探针失败");

    let g = it.lua().globals();
    let bgm: String = g.get("__probe_bgm").unwrap_or_default();
    let lang: String = g.get("__probe_lang").unwrap_or_default();
    let ty: String = g.get("__probe_type").unwrap_or_default();
    eprintln!("conf type={ty} bgm={bgm} language={lang}");

    assert_eq!(ty, "table", "conf 应为 table（fload_pluto 反序列化结果）");
    assert_eq!(bgm, "60", "conf.bgm 应往返回 60");
    assert_eq!(lang, "ja", "conf.language 应往返回 ja");
}
