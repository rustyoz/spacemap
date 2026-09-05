mod app;
mod map;

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "spacemap=info,spacemap_core=info".into()),
        )
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 920.0])
            .with_min_inner_size([900.0, 560.0])
            .with_title("Spacemap"),
        ..Default::default()
    };

    eframe::run_native(
        "Spacemap",
        options,
        Box::new(|cc| Ok(Box::new(app::SpacemapApp::new(cc)))),
    )
}
