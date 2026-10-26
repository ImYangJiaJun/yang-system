use axum::{body::Body, error_handling::HandleErrorLayer, http::{Request, StatusCode}, routing::get, BoxError, Router};
use std::{sync::{Arc, atomic::{AtomicUsize, Ordering}}, time::{Duration, Instant}};
use tokio::sync::Notify;
use tower::{limit::{ConcurrencyLimitLayer, GlobalConcurrencyLimitLayer}, load_shed::LoadShedLayer, ServiceBuilder, ServiceExt};
use tower_http::{timeout::TimeoutLayer, trace::TraceLayer};

struct Worker {
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Worker {
    async fn shutdown(mut self) {
        if let Some(task) = self.task.take() { let _ = task.await; }
    }
}
impl Drop for Worker {
    fn drop(&mut self) { if let Some(task) = self.task.take() { task.abort(); } }
}

fn request(path: &str, method: &str) -> Request<Body> {
    Request::builder().method(method).uri(path).body(Body::empty()).unwrap()
}

fn slow_router(entered: Arc<Notify>) -> Router {
    Router::new().route("/slow", get(move || {
        let entered = entered.clone();
        async move {
            entered.notify_one();
            tokio::time::sleep(Duration::from_millis(450)).await;
            "ok"
        }
    })).with_state(())
}

async fn first_then_second(app: Router, entered: Arc<Notify>) -> (StatusCode, StatusCode, u128) {
    let first_app = app.clone();
    let first = tokio::spawn(async move { first_app.oneshot(request("/slow", "GET")).await.unwrap().status() });
    entered.notified().await;
    let started = Instant::now();
    let second = app.oneshot(request("/slow", "GET")).await.unwrap().status();
    let elapsed = started.elapsed().as_millis();
    (first.await.unwrap(), second, elapsed)
}

#[tokio::main]
async fn main() {
    // 与框架相同：分别调用 Router::layer，而不是同一个 ServiceBuilder。
    let entered = Arc::new(Notify::new());
    let app = slow_router(entered.clone())
        .layer(ConcurrencyLimitLayer::new(1))
        .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_millis(650)));
    let result = first_then_second(app, entered).await;
    assert_eq!((result.0, result.1), (StatusCode::OK, StatusCode::REQUEST_TIMEOUT));
    println!("separate Router layers: first={} second={} second_elapsed_ms={} (each handler 450ms, timeout 650ms)", result.0, result.1, result.2);

    // 反例：单独叠 LoadShed 看不到内层 Route 中的 permit 等待。
    let entered = Arc::new(Notify::new());
    let app = slow_router(entered.clone()).layer(GlobalConcurrencyLimitLayer::new(1))
        .layer(ServiceBuilder::new().layer(HandleErrorLayer::new(|_: BoxError| async { StatusCode::SERVICE_UNAVAILABLE })).layer(LoadShedLayer::new()));
    let result = first_then_second(app, entered).await;
    assert_eq!((result.0, result.1), (StatusCode::OK, StatusCode::OK));
    println!("split LoadShed: first={} second={} second_elapsed_ms={}", result.0, result.1, result.2);

    let entered = Arc::new(Notify::new());
    let app = slow_router(entered.clone()).layer(ServiceBuilder::new()
        .layer(HandleErrorLayer::new(|_: BoxError| async { StatusCode::SERVICE_UNAVAILABLE }))
        .layer(LoadShedLayer::new()).layer(GlobalConcurrencyLimitLayer::new(1)));
    let result = first_then_second(app, entered).await;
    assert_eq!((result.0, result.1), (StatusCode::OK, StatusCode::SERVICE_UNAVAILABLE));
    println!("combined LoadShed: first={} second={} second_elapsed_ms={}", result.0, result.1, result.2);

    let count = Arc::new(AtomicUsize::new(0));
    let observed = count.clone();
    let app = Router::new().route("/known", get(|| async { "ok" })).with_state(()).layer(TraceLayer::new_for_http()
        .on_request(move |_: &Request<Body>, _: &tracing::Span| { observed.fetch_add(1, Ordering::SeqCst); }));
    assert_eq!(app.clone().oneshot(request("/missing", "GET")).await.unwrap().status(), StatusCode::NOT_FOUND);
    assert_eq!(app.oneshot(request("/known", "POST")).await.unwrap().status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(count.load(Ordering::SeqCst), 2);
    println!("default 404/405 TraceLayer on_request count=2");

    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let a = active.clone();
    let p = peak.clone();
    let handler = move || { let a=a.clone(); let p=p.clone(); async move {
        let n=a.fetch_add(1, Ordering::SeqCst)+1; p.fetch_max(n, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await; a.fetch_sub(1, Ordering::SeqCst); "ok"
    }};
    let app = Router::new().route("/a", get(handler.clone())).route("/b", get(handler)).with_state(()).layer(ConcurrencyLimitLayer::new(1));
    let (a,b)=tokio::join!(app.clone().oneshot(request("/a", "GET")),app.oneshot(request("/b", "GET")));
    assert!(a.unwrap().status().is_success() && b.unwrap().status().is_success());
    assert_eq!(peak.load(Ordering::SeqCst),2);
    println!("per-route max_concurrency=1: cross-route peak=2");

    let active=Arc::new(AtomicUsize::new(0));
    let peak=Arc::new(AtomicUsize::new(0));
    let a=active.clone(); let p=peak.clone();
    let handler=move || { let a=a.clone();let p=p.clone();async move {
        let n=a.fetch_add(1,Ordering::SeqCst)+1;p.fetch_max(n,Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;a.fetch_sub(1,Ordering::SeqCst);"ok"
    }};
    let app=Router::new().route("/same",get(handler.clone()).post(handler)).with_state(()).layer(ConcurrencyLimitLayer::new(1));
    let (a,b)=tokio::join!(app.clone().oneshot(request("/same","GET")),app.oneshot(request("/same","POST")));
    assert!(a.unwrap().status().is_success() && b.unwrap().status().is_success());
    assert_eq!(peak.load(Ordering::SeqCst),2);
    println!("per-method max_concurrency=1: same-path GET+POST peak=2");

    let finished=Arc::new(AtomicUsize::new(0));
    let observed=finished.clone();
    let worker=Worker { task:Some(tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        observed.fetch_add(1,Ordering::SeqCst);
    })) };
    assert!(tokio::time::timeout(Duration::from_millis(30),worker.shutdown()).await.is_err());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(finished.load(Ordering::SeqCst),1);
    println!("shutdown take-then-await + Drop abort: after timeout task still completed=1");
}
