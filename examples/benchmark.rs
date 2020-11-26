use std::time::{Duration, Instant};

use futures_util::stream::FuturesUnordered;
use futures_util::StreamExt;

/*

    A naive benchmark between actix and actix-async for exclusive message handling.
    This example serve as a way to optimize actix-async crate.

    It DOES NOT represent the real world performance of either crate.

    Build with:

    cargo build --example benchmark --release


    Run with:

    ./target/release/examples/benchmark

    optional argument: --rounds <usize> --heap-alloc <bool>

*/

pub struct ExclusiveMessage;

pub struct ConcurrentMessage;

mod actix_async_actor {
    pub use actix_async::prelude::*;
    pub use actix_rt::Arbiter;
    pub use tokio02::fs::File;
    pub use tokio02::io::AsyncReadExt;

    use super::*;

    pub struct ActixAsyncActor {
        pub file: File,
        pub heap_alloc: bool,
    }

    pub struct Tokio02Runtime;

    impl RuntimeService for Tokio02Runtime {
        type Sleep = tokio02::time::Delay;

        fn spawn<F: std::future::Future<Output = ()> + 'static>(f: F) {
            tokio02::task::spawn_local(f);
        }

        fn sleep(dur: Duration) -> Self::Sleep {
            tokio02::time::delay_for(dur)
        }
    }

    impl Actor for ActixAsyncActor {
        type Runtime = Tokio02Runtime;
    }

    message!(ExclusiveMessage, ());

    #[async_trait::async_trait(?Send)]
    impl Handler<ExclusiveMessage> for ActixAsyncActor {
        async fn handle(&self, _: ExclusiveMessage, _ctx: &Context<Self>) {}

        async fn handle_wait(&mut self, _: ExclusiveMessage, _ctx: &mut Context<Self>) {
            if self.heap_alloc {
                let mut buffer = Vec::with_capacity(100_0000);
                let _ = self.file.read(&mut buffer).await.unwrap();
            } else {
                let mut buffer = [0u8; 2_048];
                let _ = self.file.read(&mut buffer).await.unwrap();
            }
        }
    }
}

mod actix_actor {
    pub use std::cell::RefCell;
    pub use std::rc::Rc;

    pub use actix::prelude::*;
    pub use tokio02::fs::File;
    pub use tokio02::io::AsyncReadExt;

    use std::ops::DerefMut;

    use super::*;

    pub struct ActixActor {
        pub file: Rc<RefCell<File>>,
        pub heap_alloc: bool,
    }

    impl Actor for ActixActor {
        type Context = Context<Self>;
    }

    impl Message for ExclusiveMessage {
        type Result = ();
    }

    impl Handler<ExclusiveMessage> for ActixActor {
        type Result = AtomicResponse<Self, ()>;

        fn handle(&mut self, _: ExclusiveMessage, _ctx: &mut Context<Self>) -> Self::Result {
            let f = self.file.clone();
            let heap = self.heap_alloc;

            AtomicResponse::new(Box::pin(
                async move {
                    if heap {
                        let mut buffer = Vec::with_capacity(100_0000);
                        let _ = f.borrow_mut().deref_mut().read(&mut buffer).await.unwrap();
                    } else {
                        let mut buffer = [0u8; 2_048];
                        let _ = f.borrow_mut().deref_mut().read(&mut buffer).await.unwrap();
                    }
                }
                .into_actor(self),
            ))
        }
    }

    impl Message for ConcurrentMessage {
        type Result = ();
    }

    impl Handler<ConcurrentMessage> for ActixActor {
        type Result = ResponseActFuture<Self, ()>;

        fn handle(&mut self, _: ConcurrentMessage, _ctx: &mut Context<Self>) -> Self::Result {
            Box::pin(async {}.into_actor(self))
        }
    }
}

fn collect_arg(rounds: &mut usize, heap_alloc: &mut bool) -> String {
    let mut iter = std::env::args().into_iter();

    let file_path = std::env::current_dir()
        .ok()
        .and_then(|path| {
            let path = path.to_str()?.to_owned();
            Some(path + "/sample/sample.txt")
        })
        .unwrap_or_else(|| String::from("./sample/sample.txt"));

    while let Some(arg) = iter.next() {
        if arg.as_str() == "--rounds" {
            if let Some(arg) = iter.next() {
                if let Ok(r) = arg.parse::<usize>() {
                    *rounds = r;
                }
            }
        }
        if arg.as_str() == "--heap-alloc" {
            if let Some(arg) = iter.next() {
                if let Ok(use_heap) = arg.parse::<bool>() {
                    *heap_alloc = use_heap;
                }
            }
        }
    }

    file_path
}

fn main() {
    let mut rounds = 1000;
    let mut heap_alloc = false;

    let file_path = collect_arg(&mut rounds, &mut heap_alloc);

    actix::System::new("actix-async").block_on(async move {
        {
            use actix_async_actor::*;
            println!("starting benchmark actix-async");

            let mut timing = Timing::new();
            for _ in 0..10 {
                let file_path = file_path.clone();
                let addr = ActixAsyncActor::create_async(move |_| async move {
                    let file = File::open(file_path.as_str()).await.unwrap();
                    ActixAsyncActor { file, heap_alloc }
                });

                let mut exclusives = FuturesUnordered::new();
                let mut concurrents = FuturesUnordered::new();

                for _ in 0..rounds {
                    exclusives.push(addr.wait(ExclusiveMessage));
                    concurrents.push(addr.send(ExclusiveMessage));
                }

                let start = Instant::now();
                while exclusives.next().await.is_some() {}
                timing.add_exclusive(Instant::now().duration_since(start));

                let start = Instant::now();
                while concurrents.next().await.is_some() {}
                timing.add_concurrent(Instant::now().duration_since(start));
            }

            timing.print_res();
        }

        {
            use actix_actor::*;
            println!("starting benchmark actix");

            let mut timing = Timing::new();

            for _ in 0..10 {
                let file = File::open(file_path.clone()).await.unwrap();
                let heap_alloc = heap_alloc;
                let addr = ActixActor::create(move |_| ActixActor {
                    file: Rc::new(RefCell::new(file)),
                    heap_alloc,
                });

                let mut exclusives = FuturesUnordered::new();
                let mut concurrents = FuturesUnordered::new();

                for _ in 0..rounds {
                    exclusives.push(addr.send(ExclusiveMessage));
                    concurrents.push(addr.send(ConcurrentMessage));
                }

                let start = Instant::now();
                while exclusives.next().await.is_some() {}
                timing.add_exclusive(Instant::now().duration_since(start));

                let start = Instant::now();
                while concurrents.next().await.is_some() {}
                timing.add_concurrent(Instant::now().duration_since(start));
            }

            timing.print_res();
        }
    });
}

struct Timing {
    exclusive: Vec<Duration>,
    concurrent: Vec<Duration>,
}

impl Timing {
    fn new() -> Self {
        Self {
            exclusive: Vec::with_capacity(10),
            concurrent: Vec::with_capacity(10),
        }
    }

    fn add_exclusive(&mut self, dur: Duration) {
        self.exclusive.push(dur);
    }

    fn add_concurrent(&mut self, dur: Duration) {
        self.concurrent.push(dur);
    }

    fn print_res(self) {
        let dur = self
            .exclusive
            .into_iter()
            .map(|dur| dur.as_nanos())
            .sum::<u128>();
        println!(
            "average time for ExclusiveMessage: {:#?}",
            Duration::from_nanos(dur as u64) / 10
        );

        let dur = self
            .concurrent
            .into_iter()
            .map(|dur| dur.as_nanos())
            .sum::<u128>();
        println!(
            "average time for ConcurrentMessage: {:#?}",
            Duration::from_nanos(dur as u64) / 10
        );
    }
}
