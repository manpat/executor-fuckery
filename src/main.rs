#![feature(let_chains)]

use toybox::prelude::*;

mod executor;
use executor::*;


fn main() -> anyhow::Result<()> {
    toybox::run("executor", App::new)
}


struct App {
    executor: Executor,
}


impl App {
    fn new(ctx: &mut toybox::Context) -> anyhow::Result<App> {
        ctx.show_debug_menu = true;

        Ok(App {
            executor: Executor::new(),
        })
    }
}


impl toybox::App for App {
    fn present(&mut self, ctx: &mut toybox::Context) {
        self.executor.poll();

        egui::Window::new("Blah")
            .show(&ctx.egui, |ui| {
                ui.label(format!("Num tasks: {}", self.executor.active_tasks()));

                if ui.button("Spawn Task").clicked() {
                    self.executor.spawn(my_task());
                }

                if ui.button("Trigger").clicked() {
                    self.executor.trigger();
                }
            });
    }
}



async fn my_task() {
    // println!("waiting 50 frames...");

    // for _ in 0..50 {
    //     next_update().await;
    // }

    // println!("waiting for trigger or 3s timeout...");

    // when_first(
    //     on_trigger(),
    //     timeout(std::time::Duration::from_secs(3))
    // ).await;


    // println!("now waiting for trigger!");

    // on_trigger().await;

    // println!("triggered!");

    when_first(next_update(), on_trigger()).await;
    println!("triggered!");

    on_trigger().await;

    println!("woops");
}