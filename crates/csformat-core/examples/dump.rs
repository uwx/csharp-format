use csformat_core::syntax::parse_and_annotate;

fn main() {
    let src = std::fs::read_to_string(std::env::args().nth(1).unwrap()).unwrap();
    let (_syntax, tokens) = parse_and_annotate(&src);
    for t in &tokens {
        if t.is_trivia() {
            continue;
        }
        let text = t.text(&src);
        if text == "\n" {
            continue;
        }
        println!(
            "{:<12} paren={:?} comma={:?} semi={:?} colon={:?} brace={:?} flags={:#x}",
            text, t.ast.paren, t.ast.comma, t.ast.semicolon, t.ast.colon, t.ast.brace, t.ast.flags
        );
    }
}
