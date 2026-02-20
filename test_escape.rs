fn main() { println!("{:?}", escape_markdown("\\")); }

fn escape_markdown(text: &str) -> String {
    let special_chars = [
        "_", "*", "[", "]", "(", ")", "~", "`", ">", "#", "+", "-", "=", "|", "{", "}", ".", "!",
    ];
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if special_chars.contains(&c) {
            escaped.push("\\\\");
        }
        escaped.push(c);
    }
    escaped
}
