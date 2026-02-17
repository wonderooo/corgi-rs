use flate2::read::GzDecoder;

static DB_ARCHIVE: &str = "https://corgi.cardog.io/vpic.lite.db.gz";
static DB_FILE: &str = ".corgi-rs-cache/vpic.lite.db";

fn main() {
    let home_dir = dirs::home_dir().expect("home directory env variable not set");
    let db_file_path = home_dir.join(DB_FILE);

    if !db_file_path.exists() {
        if let Some(parent) = db_file_path.parent() {
            std::fs::create_dir_all(parent).expect("db directory create failure");
        }
        let mut file_writer = std::fs::File::create(db_file_path).expect("db file create failure");

        let mut archive_response = ureq::get(DB_ARCHIVE)
            .call()
            .expect("db archive download failure");
        let body_reader = archive_response.body_mut().as_reader();

        let mut gz_decoder = GzDecoder::new(body_reader);
        std::io::copy(&mut gz_decoder, &mut file_writer).expect("reader writer copy failure");
    }
}
