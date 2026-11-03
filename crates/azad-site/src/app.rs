//! The pages: the runs, a run with its goals and a timeline of their steps, and a step with its whole output. Each page
//! is rendered on the server, and then runs in the browser: a page of a run that is not done fetches it again every few
//! seconds, and a page of a running step adds the step's new lines as they come.

use leptos::prelude::*;
use leptos_meta::{Link, MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::components::{A, Route, Router, Routes};
use leptos_router::hooks::use_params_map;
use leptos_router::path;

use crate::model::{GoalView, Line, RunRow, RunView, StepPage, StepView};

/// How often the page of a run that is not done fetches it again.
const REFRESH: std::time::Duration = std::time::Duration::from_secs(3);
const STYLESHEET: &str = "/pkg/azad-site.css";
/// A small mark for the browser's tab: Azad's green, as a rounded square.
const ICON: &str = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'%3E%3Crect width='16' \
                    height='16' rx='4' fill='%231f8a4c'/%3E%3C/svg%3E";

/// The document around every page, rendered on the server.
pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <AutoReload options=options.clone()/>
                <HydrationScripts options/>
                <MetaTags/>
            </head>
            <body>
                <App/>
            </body>
        </html>
    }
}

#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();
    view! {
        <Stylesheet id="azad" href=STYLESHEET/>
        <Link rel="icon" href=ICON/>
        <Title text="Azad"/>
        <Router>
            <header>
                <A href="/" attr:class="brand">"Azad"</A>
            </header>
            <main>
                <Routes fallback=|| view! { <h1>"Not found"</h1> }>
                    <Route path=path!("") view=RunsPage/>
                    <Route path=path!("/runs/:id") view=RunPage/>
                    <Route path=path!("/runs/:id/goals/:goal/steps/:step") view=StepPageView/>
                </Routes>
            </main>
        </Router>
    }
}

#[server]
pub async fn list_runs() -> Result<Vec<RunRow>, ServerFnError> {
    crate::server::runs().map_err(ServerFnError::new)
}

#[server]
pub async fn get_run(id: u64) -> Result<RunView, ServerFnError> {
    crate::server::run(id).map_err(ServerFnError::new)
}

#[server]
pub async fn get_step(id: u64, goal: usize, step: usize) -> Result<StepPage, ServerFnError> {
    crate::server::step(id, goal, step).map_err(ServerFnError::new)
}

/// Calls `refetch` every few seconds while the page is open and `done` says no; only in the browser.
fn refresh_while(done: impl Fn() -> bool + Send + Sync + 'static, refetch: impl Fn() + Send + Sync + 'static) {
    let tick = std::sync::Arc::new(move || {
        if !done() {
            refetch();
        }
    });
    Effect::new(move |_| {
        let tick = tick.clone();
        let interval = set_interval_with_handle(move || tick(), REFRESH);
        if let Ok(handle) = interval {
            on_cleanup(move || handle.clear());
        }
    });
}

#[component]
fn RunsPage() -> impl IntoView {
    let runs = Resource::new(|| (), |_| list_runs());
    refresh_while(|| false, move || runs.refetch());
    view! {
        <Title text="Runs · Azad"/>
        <h1>"Runs"</h1>
        <Suspense fallback=|| view! { <p class="quiet">"Loading…"</p> }>
            {move || Suspend::new(async move {
                match runs.await {
                    Ok(rows) if rows.is_empty() => {
                        view! { <p class="quiet">"No runs yet: they come from GitHub's events."</p> }.into_any()
                    }
                    Ok(rows) => view! { <RunsTable rows/> }.into_any(),
                    Err(e) => view! { <p class="failure">{e.to_string()}</p> }.into_any(),
                }
            })}
        </Suspense>
    }
}

#[component]
fn RunsTable(rows: Vec<RunRow>) -> impl IntoView {
    view! {
        <table class="runs">
            <thead>
                <tr>
                    <th>"Run"</th><th></th><th>"Repository"</th><th>"Event"</th><th>"Commit"</th>
                    <th>"Goals"</th><th>"Started"</th><th>"Time"</th>
                </tr>
            </thead>
            <tbody>
                {rows.into_iter().map(|row| view! {
                    <tr>
                        <td><A href=format!("/runs/{}", row.id)>{format!("#{}", row.id)}</A></td>
                        <td><span class=format!("badge {}", row.standing.word())>{row.standing.word()}</span></td>
                        <td>{row.about.repository.clone()}</td>
                        <td>{row.about.event.clone()}</td>
                        <td><Commit about=row.about.clone()/></td>
                        <td>{row.goals.into_iter().map(|goal| view! { <code>{goal}</code>" " }).collect_view()}</td>
                        <td>{row.started}</td>
                        <td class="time">{row.took}</td>
                    </tr>
                }).collect_view()}
            </tbody>
        </table>
    }
}

#[component]
fn Commit(about: crate::model::About) -> impl IntoView {
    match about.commit_url {
        Some(url) => view! { <a href=url><code>{about.commit}</code></a> }.into_any(),
        None => view! { <code>{about.commit}</code> }.into_any(),
    }
}

#[component]
fn RunPage() -> impl IntoView {
    let params = use_params_map();
    let id = move || params.read().get("id").and_then(|id| id.parse::<u64>().ok()).unwrap_or_default();
    let run = Resource::new(id, get_run);
    let done = RwSignal::new(false);
    refresh_while(move || done.get_untracked(), move || run.refetch());
    view! {
        <Suspense fallback=|| view! { <p class="quiet">"Loading…"</p> }>
            {move || Suspend::new(async move {
                match run.await {
                    Ok(run) => {
                        done.set(run.done);
                        view! { <RunDetails run/> }.into_any()
                    }
                    Err(e) => view! { <p class="failure">{e.to_string()}</p> }.into_any(),
                }
            })}
        </Suspense>
    }
}

#[component]
fn RunDetails(run: RunView) -> impl IntoView {
    let id = run.id;
    view! {
        <Title text=format!("Run #{id} · Azad")/>
        <h1>{format!("Run #{id} ")}<span class=format!("badge {}", run.standing.word())>{run.standing.word()}</span></h1>
        <dl class="facts">
            <dt>"Repository"</dt><dd>{run.about.repository.clone()}</dd>
            <dt>"Event"</dt><dd>{run.about.event.clone()}</dd>
            <dt>"Commit"</dt><dd><Commit about=run.about.clone()/></dd>
            <dt>"Trust"</dt><dd>{run.trust.unwrap_or_default()}</dd>
            <dt>"Started"</dt><dd>{run.started.unwrap_or_default()}</dd>
            <dt>"Time"</dt><dd>{run.took.unwrap_or_default()}</dd>
        </dl>
        {run.failure.map(|failure| view! { <p class="failure">"The run could not go on: "{failure}</p> })}
        {run.goals.into_iter().enumerate().map(|(index, goal)| view! { <Goal id index goal/> }).collect_view()}
    }
}

#[component]
fn Goal(id: u64, index: usize, goal: GoalView) -> impl IntoView {
    view! {
        <section class="goal" id=format!("goal-{index}")>
            <h2>
                <span class=format!("badge {}", goal.standing.word())>{goal.standing.word()}</span>" "
                <code>{goal.name.clone()}</code>" "<span class="quiet">{goal.title.clone()}</span>
            </h2>
            <p class="metrics">{goal.metrics.clone()}</p>
            <table class="steps">
                <thead>
                    <tr><th></th><th>"Step"</th><th>"Result"</th><th>"Time"</th><th class="timeline">"Timeline"</th></tr>
                </thead>
                <tbody>
                    {goal.steps.into_iter().enumerate().map(|(step_index, step)| {
                        let link = format!("/runs/{id}/goals/{index}/steps/{step_index}");
                        view! { <StepRow step link/> }
                    }).collect_view()}
                </tbody>
            </table>
        </section>
    }
}

fn result_text(step: &StepView) -> String {
    match &step.reason {
        Some(reason) => format!("{}: {}", step.state, reason.lines().next().unwrap_or_default()),
        None => step.state.clone(),
    }
}

#[component]
fn StepRow(step: StepView, link: String) -> impl IntoView {
    let result = result_text(&step);
    let bar = step.bar.map(|(left, width)| {
        let style = format!("margin-left: {left:.2}%; width: {width:.2}%");
        view! { <div class="track"><div class=format!("bar {}", step.state) style=style></div></div> }
    });
    view! {
        <tr class=step.state.clone()>
            <td>{step.icon.clone()}</td>
            <td><A href=link><code>{step.name.clone()}</code></A></td>
            <td>{result}</td>
            <td class="time">{step.time.clone()}</td>
            <td class="timeline">{bar}</td>
        </tr>
    }
}

#[component]
fn StepPageView() -> impl IntoView {
    let params = use_params_map();
    let number =
        move |name: &str| params.read().get(name).and_then(|value| value.parse::<usize>().ok()).unwrap_or_default();
    let key = move || (number("id") as u64, number("goal"), number("step"));
    let resource = Resource::new(key, |(id, goal, step)| get_step(id, goal, step));
    view! {
        <Suspense fallback=|| view! { <p class="quiet">"Loading…"</p> }>
            {move || Suspend::new(async move {
                match resource.await {
                    Ok(page) => view! { <StepDetails page on_end=move || resource.refetch()/> }.into_any(),
                    Err(e) => view! { <p class="failure">{e.to_string()}</p> }.into_any(),
                }
            })}
        </Suspense>
    }
}

#[component]
fn StepDetails(page: StepPage, on_end: impl Fn() + Send + Sync + 'static) -> impl IntoView {
    let StepPage { run, goal_index, step_index, goal, step, lines, offset, millis } = page;
    let lines = RwSignal::new(lines);
    let millis = RwSignal::new(millis);
    if !step.ended {
        follow(format!("/events/runs/{run}/goals/{goal_index}/steps/{step_index}"), offset, lines, on_end);
        tick(millis);
    }
    let empty = match (step.state.as_str(), step.ended) {
        ("cached", _) => "The result came from the cache, so the step did not run now.",
        (_, false) => "No output yet.",
        _ => "The step wrote nothing.",
    };
    let state = step.state.clone();
    let shown = move || match millis.get() {
        Some(millis) => format!("{state} · {}", duration(millis)),
        None => state.clone(),
    };
    view! {
        <Title text=format!("{} · Azad", step.name)/>
        <p class="crumbs">
            <A href=format!("/runs/{run}")>{format!("Run #{run}")}</A>" › "
            <A href=format!("/runs/{run}#goal-{goal_index}")><code>{goal}</code></A>
        </p>
        <h1>{step.icon.clone()}" "<code>{step.name.clone()}</code>" "<span class="quiet">{shown}</span></h1>
        {step.reason.clone().map(|reason| view! { <p class="failure">{reason}</p> })}
        <Show when=move || lines.with(Vec::is_empty)>
            <p class="quiet">{empty}</p>
        </Show>
        <pre class="log">
            <For each=move || lines.get().into_iter().enumerate() key=|(number, _)| *number let((_, line))>
                <LogLine line/>
            </For>
        </pre>
    }
}

/// Counts a running step's time on, a second at a time, in the browser.
fn tick(millis: RwSignal<Option<u64>>) {
    Effect::new(move |_| {
        let second = std::time::Duration::from_secs(1);
        let tick = move || millis.update(|millis| *millis = Some(millis.unwrap_or_default() + 1000));
        if let Ok(handle) = set_interval_with_handle(tick, second) {
            on_cleanup(move || handle.clear());
        }
    });
}

/// A time in milliseconds, for people, as the orchestrator writes it: `35ms`, `4.2s`, or `3m 5s`.
fn duration(millis: u64) -> String {
    let seconds = millis / 1000;
    match (seconds, seconds < 60) {
        (0, _) => format!("{millis}ms"),
        (_, true) => format!("{seconds}.{}s", (millis % 1000) / 100),
        (_, false) => format!("{}m {}s", seconds / 60, seconds % 60),
    }
}

#[component]
fn LogLine(line: Line) -> impl IntoView {
    view! {
        <span class="line">
            {line.into_iter().map(|segment| view! { <span class=segment.class>{segment.text}</span> }).collect_view()}
        </span>
        "\n"
    }
}

/// Adds a running step's new lines to `lines` as the server sends them, from the record's byte `offset` on, and calls
/// `on_end` when the step ends. Only the browser follows; the server renders what there is.
#[cfg(feature = "hydrate")]
fn follow(url: String, offset: u64, lines: RwSignal<Vec<Line>>, on_end: impl Fn() + Send + Sync + 'static) {
    use send_wrapper::SendWrapper;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;
    use web_sys::{EventSource, MessageEvent};

    let Ok(source) = EventSource::new(&format!("{url}?offset={offset}")) else { return };
    let on_line = Closure::<dyn Fn(MessageEvent)>::new(move |event: MessageEvent| {
        let Some(data) = event.data().as_string() else { return };
        if let Ok(line) = serde_json::from_str::<Line>(&data) {
            lines.update(|lines| lines.push(line));
        }
    });
    let ended = source.clone();
    let on_ended = Closure::<dyn Fn(MessageEvent)>::new(move |_: MessageEvent| {
        ended.close();
        on_end();
    });
    source.add_event_listener_with_callback(crate::LINE_EVENT, on_line.as_ref().unchecked_ref()).ok();
    source.add_event_listener_with_callback(crate::END_EVENT, on_ended.as_ref().unchecked_ref()).ok();
    let kept = SendWrapper::new((source, on_line, on_ended));
    on_cleanup(move || kept.0.close());
}

#[cfg(not(feature = "hydrate"))]
fn follow(_url: String, _offset: u64, _lines: RwSignal<Vec<Line>>, _on_end: impl Fn() + Send + Sync + 'static) {}
