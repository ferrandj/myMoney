pub mod pdf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "pdf-words" {
        let data = std::fs::read(&args[2]).expect("read");
        let tol: f64 = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(1.0);
        let pages = pdf::extract_words(&data, tol).expect("pdf");
        println!("{}", serde_json::to_string(&pages).unwrap());
    } else if args.len() >= 3 && args[1] == "pdf-text" {
        let data = std::fs::read(&args[2]).expect("read");
        print!("{}", pdf::extract_text(&data).expect("pdf"));
    }
}
