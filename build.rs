use std::path::Path;

use flate2::read::GzDecoder;

static DB_ARCHIVE: &str = "https://corgi.cardog.io/vpic.lite.db.gz";
static DB_FILE: &str = "vpic.lite.db";

fn main() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let db_file_path = Path::new(&out_dir).join(DB_FILE);

    if !db_file_path.exists() {
        let mut file_writer =
            std::fs::File::create(db_file_path.clone()).expect("db file create failure");

        let mut archive_response = ureq::get(DB_ARCHIVE)
            .call()
            .expect("db archive download failure");
        let body_reader = archive_response.body_mut().as_reader();

        let mut gz_decoder = GzDecoder::new(body_reader);
        std::io::copy(&mut gz_decoder, &mut file_writer).expect("reader writer copy failure");
    }

    println!("cargo:rustc-env=DATABASE_URL={}", db_file_path.display());
}
