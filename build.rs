use dotenv::dotenv;

fn main() {
    dotenv().ok();
    if let Ok(url) = std::env::var("DATABASE_URL") {
        println!("cargo:rustc-env=DATABASE_URL={url}");
    }
}
