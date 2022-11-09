//! Upgrade command.
//! It updates the application from version v1.0.0 to v2.0.0.
//! You can execute it with: `cargo run --bin upgrade ./data_v2.db ./uploads`

use torrust_index_backend::upgrades::from_v1_0_0_to_v2_0_0::upgrader::upgrade;

#[actix_web::main]
async fn main() {
    upgrade().await;
}
