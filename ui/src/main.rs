use dioxus::prelude::*;

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        main { class: "app",
            h1 { "adjourn" }
            p { "untimed correspondence chess" }
        }
    }
}
