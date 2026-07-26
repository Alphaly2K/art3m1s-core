use asb_interpreter::{Interpreter, InterpreterConfig};
fn main() {
    let mut interp = Interpreter::new(InterpreterConfig::default());
    let data = std::fs::read("/Users/alphaly/lfpm/hamidashi/system/script.asb").unwrap();
    interp.load_asb("system/script.asb", &data).unwrap();
    let s = interp.get_script("system/script.asb").unwrap();
    // reverse label map
    let mut by_line: std::collections::HashMap<usize, Vec<String>> = Default::default();
    for (name, line) in &s.labels {
        by_line.entry(*line).or_default().push(name.clone());
    }
    for (idx, ins) in s.instructions.iter().enumerate() {
        if idx >= 95 && idx <= 120 {
            let lbls = by_line
                .get(&idx)
                .map(|v| format!("  <labels: {:?}>", v))
                .unwrap_or_default();
            println!(
                "{:>4}: tag={} fn={:?} params={:?}{}",
                idx,
                ins.tag,
                ins.get("function"),
                ins.params,
                lbls
            );
        }
    }
}
