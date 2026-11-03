//! `azad-site`: the site of Azad's orchestrator (decision 0034). It serves the pages of the runs in the orchestrator's
//! state, which it only reads, at the address that `LEPTOS_SITE_ADDR` or the crate's Leptos settings name, with the
//! browser's half (`LEPTOS_SITE_ROOT`) from the files that `cargo leptos build` made.

use std::path::PathBuf;
use std::process::ExitCode;

use axum::Router;
use axum::routing::get;
use azad_site::app::{App, shell};
use azad_site::server;
use leptos::prelude::*;
use leptos_axum::{LeptosRoutes, generate_route_list};

const STATE_ENV: &str = "AZAD_ORCHESTRATOR_STATE";
const EVENTS_ROUTE: &str = "/events/runs/{id}/goals/{goal}/steps/{step}";

#[tokio::main]
async fn main() -> ExitCode {
    match serve().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("azad-site: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn serve() -> Result<(), String> {
    let state = std::env::var_os(STATE_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| format!("set {STATE_ENV} to the orchestrator's state: <store>/orchestrator"))?;
    server::set_state(state);
    let configuration = get_configuration(None).map_err(|e| e.to_string())?;
    let options = configuration.leptos_options;
    let address = options.site_addr;
    let routes = generate_route_list(App);
    let app = Router::new()
        .route(EVENTS_ROUTE, get(server::follow))
        .leptos_routes(&options, routes, {
            let options = options.clone();
            move || shell(options.clone())
        })
        .fallback(leptos_axum::file_and_error_handler(shell))
        .with_state(options);
    let listener = tokio::net::TcpListener::bind(&address).await.map_err(|e| format!("listening on {address}: {e}"))?;
    eprintln!("azad-site: http://{address}");
    axum::serve(listener, app.into_make_service()).await.map_err(|e| e.to_string())
}
