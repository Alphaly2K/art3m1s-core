use asb_interpreter::{Interpreter, InterpreterConfig};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| {
        eprintln!("usage: dump_asb <script.asb> [start-line] [end-line]");
        std::process::exit(2);
    });
    let start = args
        .next()
        .map(|value| value.parse::<usize>().expect("invalid start line"))
        .unwrap_or(0);
    let end = args
        .next()
        .map(|value| value.parse::<usize>().expect("invalid end line"))
        .unwrap_or(usize::MAX);

    let mut interp = Interpreter::new(InterpreterConfig::default());
    let data = std::fs::read(&path).unwrap_or_else(|error| panic!("read {path}: {error}"));
    interp.load_asb(&path, &data).unwrap();
    let s = interp.get_script(&path).unwrap();
    // reverse label map
    let mut by_line: std::collections::HashMap<usize, Vec<String>> = Default::default();
    for (name, line) in &s.labels {
        by_line.entry(*line).or_default().push(name.clone());
    }
    for (idx, ins) in s.instructions.iter().enumerate() {
        if idx >= start && idx <= end {
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
