//! Light/dark theme: dark by default, follows the OS setting live, and stops
//! following once the person picks one with the toggle. The light palette is
//! `body.light` in the stylesheet; this only decides when that class is on.
//! Same UX as ShipScreen and GitAgent.

use dioxus::prelude::*;

/// Floating top-right sun/moon button. Render it once, at the app root.
#[component]
pub fn ThemeToggle() -> Element {
    let mut is_light = use_signal(|| false);
    let mut overridden = use_signal(|| false);

    // The webview reports the OS appearance, and reports it again whenever it
    // changes, so there is nothing to poll.
    use_future(move || async move {
        let mut os = document::eval(
            "const mq = window.matchMedia('(prefers-color-scheme: light)');
             dioxus.send(mq.matches);
             mq.addEventListener('change', (e) => dioxus.send(e.matches));
             await new Promise(() => {});",
        );
        while let Ok(light) = os.recv::<bool>().await {
            if !*overridden.peek() {
                is_light.set(light);
            }
        }
    });

    use_effect(move || {
        document::eval(&format!(
            "document.body.classList.toggle('light', {});",
            is_light()
        ));
    });

    let label = if is_light() { "Switch to dark mode" } else { "Switch to light mode" };
    rsx! {
        button {
            class: "btn-theme",
            title: label,
            aria_label: label,
            onclick: move |_| {
                overridden.set(true);
                let next = !is_light();
                is_light.set(next);
            },
            svg { class: "theme-icon", view_box: "0 0 24 24", "aria-hidden": "true",
                if is_light() {
                    path { d: "M20.5 13.3A8.5 8.5 0 1110.7 3.5a6.6 6.6 0 009.8 9.8z" }
                } else {
                    path { d: "M12 4V2M12 22v-2M4 12H2M22 12h-2M5.6 5.6L4.2 4.2M19.8 19.8l-1.4-1.4M18.4 5.6l1.4-1.4M4.2 19.8l1.4-1.4" }
                    path { d: "M12 7.5a4.5 4.5 0 100 9 4.5 4.5 0 000-9z" }
                }
            }
        }
    }
}
