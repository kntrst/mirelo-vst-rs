//! Entry point for standalone mode

use mirelo_vst_rs::Plugin;

fn main() {
    truce_standalone::run::<Plugin>();
}
