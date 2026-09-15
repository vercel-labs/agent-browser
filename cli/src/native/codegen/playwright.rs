use super::{
    single_element_target, ClickKind, NavigationKind, PointerKind, Scope, SelectorKind, Step,
    Target,
};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatIssue {
    pub code: &'static str,
    pub message: &'static str,
    pub step_index: usize,
    pub omitted: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderedPlaywright {
    pub output: String,
    pub emitted: usize,
    pub issues: Vec<FormatIssue>,
}

fn js(value: &str) -> String {
    serde_json::to_string(value).expect("a Rust string is valid JSON")
}

/// `single` marks a step that acted on one element. Playwright refuses a
/// locator that matches several, so an unverified selector gets `.first()`,
/// which is what the command itself did when the browser resolved it.
fn locator(target: &Target, page: &str, frame: &[usize], single: bool) -> Option<String> {
    let mut root = page.to_string();
    for index in frame {
        root.push_str(&format!(".frameLocator('iframe, frame').nth({index})"));
    }
    // An older journal has no `verified` flag. A leading test ID came from the
    // probe, which reports one only when it is unique.
    let verified = target.verified
        || matches!(target.selectors.first(), Some(SelectorKind::TestId { value }) if !value.is_empty());
    let suffix = if single && !verified { ".first()" } else { "" };
    target.selectors.iter().find_map(|selector| match selector {
        SelectorKind::TestId { value } if !value.is_empty() => {
            Some(format!("{root}.getByTestId({})", js(value)))
        }
        SelectorKind::Role { role, name, nth } if !role.is_empty() && !name.is_empty() => {
            // Capture resolved the ref by exact role and name. Partial matching
            // would let `Save` match `Save draft`, and it would also change
            // what a stored `nth` counts.
            let mut result = format!(
                "{root}.getByRole({}, {{ name: {}, exact: true }})",
                js(role),
                js(name)
            );
            if let Some(nth) = nth {
                result.push_str(&format!(".nth({nth})"));
            }
            Some(result)
        }
        SelectorKind::Css { value } if !value.is_empty() => {
            Some(format!("{root}.locator({}){suffix}", js(value)))
        }
        SelectorKind::XPath { value } if !value.is_empty() => Some(format!(
            "{root}.locator({}){suffix}",
            js(&format!("xpath={value}"))
        )),
        _ => None,
    })
}

fn logical_number(page_id: &str) -> Option<usize> {
    page_id.strip_prefix('p')?.parse().ok()
}

fn variable_for(page_id: &str) -> String {
    if page_id == "main" || page_id == "p1" {
        "page".to_string()
    } else if let Some(number) = logical_number(page_id) {
        format!("page{number}")
    } else {
        format!(
            "page_{}",
            page_id.replace(|character: char| !character.is_ascii_alphanumeric(), "_")
        )
    }
}

fn page_for(scope: &Scope, pages: &mut HashMap<String, String>, lines: &mut Vec<String>) -> String {
    if scope.target == "main" || scope.target == "p1" {
        return "page".to_string();
    }
    if let Some(page) = pages.get(&scope.target) {
        return page.clone();
    }
    let page = variable_for(&scope.target);
    lines.push(format!("  const {page} = await context.newPage();"));
    pages.insert(scope.target.clone(), page.clone());
    page
}

fn assertion(lines: &mut Vec<String>, page: &str, url: &Option<String>) {
    if let Some(url) = url {
        lines.push(format!("  await expect({page}).toHaveURL({});", js(url)));
    }
}

fn initial_viewport(steps: &[Step]) -> Option<(i64, i64, f64, bool)> {
    steps.iter().find_map(|step| match step {
        Step::SetViewport {
            width,
            height,
            device_scale_factor,
            is_mobile,
        } => Some((*width, *height, *device_scale_factor, *is_mobile)),
        Step::ScopedViewport {
            width,
            height,
            device_scale_factor,
            is_mobile,
            scope: Scope { target, .. },
        } if target == "main" || target == "p1" => {
            Some((*width, *height, *device_scale_factor, *is_mobile))
        }
        _ => None,
    })
}

pub fn render_playwright_with_report(title: &str, steps: &[Step]) -> RenderedPlaywright {
    let viewport = initial_viewport(steps);
    let mut lines = vec!["import { test, expect } from '@playwright/test';".to_string()];
    if let Some((width, height, scale, mobile)) = viewport {
        lines.push(String::new());
        lines.push(format!(
            "test.use({{ viewport: {{ width: {width}, height: {height} }}, deviceScaleFactor: {scale}, isMobile: {mobile}, hasTouch: {mobile} }});"
        ));
    }
    lines.extend([
        String::new(),
        format!("test({}, async ({{ page, context }}) => {{", js(title)),
    ]);

    let mut pages = HashMap::from([("p1".to_string(), "page".to_string())]);
    // `test.use` already puts the main page at the initial viewport.
    let mut current_viewport = HashMap::new();
    if let Some(size) = viewport {
        current_viewport.insert("p1".to_string(), size);
    }
    let mut issues = Vec::new();
    let mut emitted = 0usize;
    for (step_index, step) in steps.iter().enumerate() {
        let before = lines.len();
        let unverified_target = single_element_target(step).is_some_and(|target| !target.verified);
        match step {
            Step::SetViewport { width, height, .. } => {
                if viewport.is_none() {
                    lines.push(format!(
                        "  await page.setViewportSize({{ width: {width}, height: {height} }});"
                    ));
                }
            }
            Step::Navigate { url } => lines.push(format!("  await page.goto({});", js(url))),
            Step::Click {
                target,
                count,
                kind,
                opens_popup,
                scope,
                asserted_url,
            } => {
                let page = page_for(scope, &mut pages, &mut lines);
                let Some(loc) = locator(target, &page, &scope.frame, true) else {
                    issues.push(FormatIssue {
                        code: "playwright-unsafe-selector",
                        message: "Playwright omitted an action without a safe selector.",
                        step_index,
                        omitted: true,
                    });
                    continue;
                };
                let popup_number = step_index + 1;
                if *opens_popup {
                    lines.push(format!(
                        "  const popupPromise{popup_number} = {page}.waitForEvent('popup');"
                    ));
                }
                let operation = match kind {
                    ClickKind::Check => "check()".to_string(),
                    ClickKind::Uncheck => "uncheck()".to_string(),
                    ClickKind::Tap => "tap()".to_string(),
                    ClickKind::Click if *count == 2 => "dblclick()".to_string(),
                    ClickKind::Click => "click()".to_string(),
                };
                lines.push(format!("  await {loc}.{operation};"));
                let asserted_page = if *opens_popup {
                    let popup = format!("popup{popup_number}");
                    lines.push(format!(
                        "  const {popup} = await popupPromise{popup_number};"
                    ));
                    popup
                } else {
                    page
                };
                assertion(&mut lines, &asserted_page, asserted_url);
            }
            Step::Hover { target, scope } => {
                let page = page_for(scope, &mut pages, &mut lines);
                if let Some(loc) = locator(target, &page, &scope.frame, true) {
                    lines.push(format!("  await {loc}.hover();"));
                } else {
                    issues.push(FormatIssue {
                        code: "playwright-unsafe-selector",
                        message: "Playwright omitted an action without a safe selector.",
                        step_index,
                        omitted: true,
                    });
                }
            }
            Step::Change {
                target,
                value,
                is_select,
                scope,
                asserted_url,
            } => {
                let page = page_for(scope, &mut pages, &mut lines);
                if let Some(loc) = locator(target, &page, &scope.frame, true) {
                    let operation = if *is_select { "selectOption" } else { "fill" };
                    lines.push(format!("  await {loc}.{operation}({});", js(value)));
                    assertion(&mut lines, &page, asserted_url);
                }
            }
            Step::KeyDown { key, scope } => {
                let page = page_for(scope, &mut pages, &mut lines);
                lines.push(format!("  await {page}.keyboard.down({});", js(key)));
            }
            Step::KeyUp { key, scope } => {
                let page = page_for(scope, &mut pages, &mut lines);
                lines.push(format!("  await {page}.keyboard.up({});", js(key)));
            }
            Step::Scroll { x, y, scope, .. } => {
                let page = page_for(scope, &mut pages, &mut lines);
                lines.push(format!("  await {page}.mouse.wheel({x}, {y});"));
            }
            Step::WaitForElement {
                target,
                visible,
                properties,
                count,
                scope,
                ..
            } => {
                let page = page_for(scope, &mut pages, &mut lines);
                // A count assertion matches several elements on purpose.
                let Some(loc) = locator(target, &page, &scope.frame, count.is_none()) else {
                    issues.push(FormatIssue {
                        code: "playwright-unsafe-selector",
                        message: "Playwright omitted an assertion without a safe selector.",
                        step_index,
                        omitted: true,
                    });
                    continue;
                };
                if let Some(visible) = visible {
                    lines.push(if *visible {
                        format!("  await expect({loc}).toBeVisible();")
                    } else {
                        format!("  await expect({loc}).toBeHidden();")
                    });
                }
                if let Some(checked) = properties.get("checked").and_then(|value| value.as_bool()) {
                    lines.push(if checked {
                        format!("  await expect({loc}).toBeChecked();")
                    } else {
                        format!("  await expect({loc}).not.toBeChecked();")
                    });
                }
                if let Some(disabled) = properties.get("disabled").and_then(|value| value.as_bool())
                {
                    lines.push(if disabled {
                        format!("  await expect({loc}).toBeDisabled();")
                    } else {
                        format!("  await expect({loc}).toBeEnabled();")
                    });
                }
                if let Some(count) = count {
                    lines.push(format!("  await expect({loc}).toHaveCount({count});"));
                }
            }
            Step::Close => {}
            Step::NewTab { url } => {
                let page = format!("page{}", step_index + 2);
                lines.push(format!("  const {page} = await context.newPage();"));
                if let Some(url) = url {
                    lines.push(format!("  await {page}.goto({});", js(url)));
                }
            }
            Step::ScopedViewport {
                width,
                height,
                device_scale_factor,
                is_mobile,
                scope,
            } => {
                let page = page_for(scope, &mut pages, &mut lines);
                // Skip a step that changes nothing for this page. Comparing
                // with the initial viewport instead would drop a resize back to
                // the size the recording started at.
                let key = if scope.target == "main" {
                    "p1".to_string()
                } else {
                    scope.target.clone()
                };
                let size = (*width, *height, *device_scale_factor, *is_mobile);
                if current_viewport.get(&key) == Some(&size) {
                    continue;
                }
                if viewport.is_some_and(|(_, _, scale, mobile)| {
                    scale != *device_scale_factor || mobile != *is_mobile
                }) {
                    issues.push(FormatIssue {
                        code: "playwright-context-mode-change-omitted",
                        message: "Playwright cannot change mobile or device scale state on an existing context.",
                        step_index,
                        omitted: true,
                    });
                    continue;
                }
                lines.push(format!(
                    "  await {page}.setViewportSize({{ width: {width}, height: {height} }});"
                ));
                current_viewport.insert(key, size);
            }
            Step::ScopedNavigation { kind, url, scope } => {
                let page = page_for(scope, &mut pages, &mut lines);
                lines.push(match kind {
                    NavigationKind::Goto => format!("  await {page}.goto({});", js(url)),
                    NavigationKind::Back => format!("  await {page}.goBack();"),
                    NavigationKind::Forward => format!("  await {page}.goForward();"),
                    NavigationKind::Reload => format!("  await {page}.reload();"),
                });
            }
            Step::Pointer {
                target,
                kind,
                pointer,
                button,
                count,
                position,
                opens_popup,
                popup_page,
                scope,
                asserted_url,
            } => {
                let page = page_for(scope, &mut pages, &mut lines);
                let Some(loc) = locator(target, &page, &scope.frame, true) else {
                    issues.push(FormatIssue {
                        code: "playwright-unsafe-selector",
                        message: "Playwright omitted an action without a safe selector.",
                        step_index,
                        omitted: true,
                    });
                    continue;
                };
                let popup_id = popup_page
                    .clone()
                    .unwrap_or_else(|| format!("p{}", step_index + 2));
                let popup_number = logical_number(&popup_id).unwrap_or(step_index + 2);
                if *opens_popup {
                    lines.push(format!(
                        "  const popupPromise{popup_number} = {page}.waitForEvent('popup');"
                    ));
                }
                let position_option = position
                    .map(|(x, y)| format!(", position: {{ x: {x}, y: {y} }}"))
                    .unwrap_or_default();
                let operation = match (pointer, kind) {
                    (PointerKind::Touch, _) => position
                        .map(|(x, y)| format!("tap({{ position: {{ x: {x}, y: {y} }} }})"))
                        .unwrap_or_else(|| "tap()".to_string()),
                    (_, ClickKind::Check) => "check()".to_string(),
                    (_, ClickKind::Uncheck) => "uncheck()".to_string(),
                    (_, ClickKind::Tap) => "tap()".to_string(),
                    (_, ClickKind::Click) if *count == 2 => {
                        format!("dblclick({{ button: {}{position_option} }})", js(button))
                    }
                    _ => format!("click({{ button: {}{position_option} }})", js(button)),
                };
                lines.push(format!("  await {loc}.{operation};"));
                let asserted_page = if *opens_popup {
                    let popup = variable_for(&popup_id);
                    lines.push(format!(
                        "  const {popup} = await popupPromise{popup_number};"
                    ));
                    pages.insert(popup_id, popup.clone());
                    popup
                } else {
                    page
                };
                assertion(&mut lines, &asserted_page, asserted_url);
            }
            Step::Fill {
                target,
                value,
                scope,
                asserted_url,
            }
            | Step::SetValue {
                target,
                value,
                scope,
                asserted_url,
            } => {
                let page = page_for(scope, &mut pages, &mut lines);
                if let Some(loc) = locator(target, &page, &scope.frame, true) {
                    lines.push(format!("  await {loc}.fill({});", js(value)));
                    assertion(&mut lines, &page, asserted_url);
                }
            }
            Step::Type {
                target,
                text,
                clear,
                delay_ms,
                scope,
                asserted_url,
            } => {
                let page = page_for(scope, &mut pages, &mut lines);
                if let Some(loc) = locator(target, &page, &scope.frame, true) {
                    if *clear {
                        lines.push(format!("  await {loc}.fill(\"\");"));
                    }
                    let options = delay_ms
                        .map(|delay| format!(", {{ delay: {delay} }}"))
                        .unwrap_or_default();
                    lines.push(format!(
                        "  await {loc}.pressSequentially({}{options});",
                        js(text)
                    ));
                    assertion(&mut lines, &page, asserted_url);
                }
            }
            Step::Select {
                target,
                values,
                scope,
                asserted_url,
            } => {
                let page = page_for(scope, &mut pages, &mut lines);
                if let Some(loc) = locator(target, &page, &scope.frame, true) {
                    let values =
                        serde_json::to_string(values).expect("string arrays are valid JSON");
                    lines.push(format!("  await {loc}.selectOption({values});"));
                    assertion(&mut lines, &page, asserted_url);
                }
            }
            Step::Press {
                modifiers,
                key,
                scope,
                asserted_url,
            } => {
                let page = page_for(scope, &mut pages, &mut lines);
                let chord = modifiers
                    .iter()
                    .chain(std::iter::once(key))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("+");
                lines.push(format!("  await {page}.keyboard.press({});", js(&chord)));
                assertion(&mut lines, &page, asserted_url);
            }
            Step::Wheel {
                target,
                x,
                y,
                scope,
                ..
            } => {
                let page = page_for(scope, &mut pages, &mut lines);
                if let Some(target) = target {
                    if let Some(loc) = locator(target, &page, &scope.frame, true) {
                        lines.push(format!("  await {loc}.evaluate((element, delta) => element.scrollBy(delta.x, delta.y), {{ x: {x}, y: {y} }});"));
                    }
                } else {
                    lines.push(format!("  await {page}.mouse.wheel({x}, {y});"));
                }
            }
            Step::Upload {
                target,
                paths,
                scope,
            } => {
                let page = page_for(scope, &mut pages, &mut lines);
                if let Some(loc) = locator(target, &page, &scope.frame, true) {
                    let paths = serde_json::to_string(paths).expect("string arrays are valid JSON");
                    lines.push(format!("  await {loc}.setInputFiles({paths});"));
                }
            }
            Step::NewPage { url, scope } => {
                let page = variable_for(&scope.target);
                if !pages.contains_key(&scope.target) {
                    lines.push(format!("  const {page} = await context.newPage();"));
                    pages.insert(scope.target.clone(), page.clone());
                }
                if let Some(url) = url {
                    lines.push(format!("  await {page}.goto({});", js(url)));
                }
            }
            Step::OpenPage { url, scope, .. } => {
                let page = variable_for(&scope.target);
                if !pages.contains_key(&scope.target) {
                    lines.push(format!("  const {page} = await context.newPage();"));
                    pages.insert(scope.target.clone(), page.clone());
                }
                lines.push(format!("  await {page}.goto({});", js(url)));
            }
            Step::ClosePage { scope } => {
                let page = page_for(scope, &mut pages, &mut lines);
                lines.push(format!("  await {page}.close();"));
            }
        }
        if lines.len() > before {
            emitted += 1;
            // Report only for a step that reached the page. A step omitted for
            // another reason already carries its own issue.
            if unverified_target {
                issues.push(FormatIssue {
                    code: "playwright-target-not-unique",
                    message: "Playwright used the first match, because the recorded selector was not verified to address one element.",
                    step_index,
                    omitted: false,
                });
            }
        }
    }
    lines.push("});".to_string());
    lines.push(String::new());
    RenderedPlaywright {
        output: lines.join("\n"),
        emitted,
        issues,
    }
}

pub fn render_playwright(title: &str, steps: &[Step]) -> String {
    render_playwright_with_report(title, steps).output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(selector: SelectorKind) -> Target {
        Target {
            selectors: vec![selector],
            input_type: None,
            verified: true,
        }
    }

    #[test]
    fn uses_json_strings_and_typed_selectors() {
        let rendered = render_playwright(
            "line\nseparator\u{2028}",
            &[
                Step::Fill {
                    target: target(SelectorKind::XPath {
                        value: "//input".into(),
                    }),
                    value: "a\nb".into(),
                    scope: Scope::default(),
                    asserted_url: None,
                },
                Step::Hover {
                    target: target(SelectorKind::Css {
                        value: "#item".into(),
                    }),
                    scope: Scope::default(),
                },
            ],
        );
        assert!(rendered.contains("locator(\"xpath=//input\")"));
        assert!(rendered.contains("fill(\"a\\nb\")"));
        assert!(rendered.contains("locator(\"#item\")"));
    }

    #[test]
    fn renders_context_actions_pages_and_two_unique_popups() {
        let click = |popup: &str| Step::Pointer {
            target: target(SelectorKind::Css {
                value: "#open".into(),
            }),
            kind: ClickKind::Click,
            pointer: PointerKind::Mouse,
            button: "left".into(),
            count: 1,
            position: None,
            opens_popup: true,
            popup_page: Some(popup.into()),
            scope: Scope {
                target: "p1".into(),
                frame: Vec::new(),
                page_url: None,
            },
            asserted_url: None,
        };
        let rendered = render_playwright(
            "flow",
            &[
                Step::ScopedViewport {
                    width: 390,
                    height: 844,
                    device_scale_factor: 3.0,
                    is_mobile: true,
                    scope: Scope {
                        target: "p1".into(),
                        frame: Vec::new(),
                        page_url: None,
                    },
                },
                click("p2"),
                click("p3"),
                Step::Select {
                    target: target(SelectorKind::Css {
                        value: "select".into(),
                    }),
                    values: vec!["a".into(), "b".into()],
                    scope: Scope {
                        target: "p2".into(),
                        frame: Vec::new(),
                        page_url: None,
                    },
                    asserted_url: None,
                },
                Step::Upload {
                    target: target(SelectorKind::Css {
                        value: "input".into(),
                    }),
                    paths: vec!["a.txt".into()],
                    scope: Scope {
                        target: "p3".into(),
                        frame: Vec::new(),
                        page_url: None,
                    },
                },
            ],
        );
        assert!(rendered.contains("test.use({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true })"));
        assert!(rendered.contains("const page2 = await popupPromise2"));
        assert!(rendered.contains("const page3 = await popupPromise3"));
        assert!(rendered.contains("selectOption([\"a\",\"b\"])"));
        assert!(rendered.contains("setInputFiles([\"a.txt\"])"));
    }

    #[test]
    fn production_renderer_matches_shared_playwright_fixture() {
        let steps = vec![
            Step::ScopedViewport {
                width: 390,
                height: 844,
                device_scale_factor: 3.0,
                is_mobile: true,
                scope: Scope {
                    target: "p1".into(),
                    frame: Vec::new(),
                    page_url: None,
                },
            },
            Step::ScopedNavigation {
                kind: NavigationKind::Goto,
                url: "https://example.com/start\nnext".into(),
                scope: Scope {
                    target: "p1".into(),
                    frame: Vec::new(),
                    page_url: None,
                },
            },
            Step::Fill {
                target: target(SelectorKind::XPath {
                    value: "//input[@name='q']".into(),
                }),
                value: "line one\nline two".into(),
                scope: Scope {
                    target: "p1".into(),
                    frame: vec![0, 1],
                    page_url: None,
                },
                asserted_url: None,
            },
            Step::Type {
                target: target(SelectorKind::Css {
                    value: "#type".into(),
                }),
                text: "suffix".into(),
                clear: true,
                delay_ms: Some(17),
                scope: Scope {
                    target: "p1".into(),
                    frame: Vec::new(),
                    page_url: None,
                },
                asserted_url: None,
            },
            Step::Select {
                target: target(SelectorKind::Css {
                    value: "select".into(),
                }),
                values: vec!["a".into(), "b".into()],
                scope: Scope {
                    target: "p1".into(),
                    frame: Vec::new(),
                    page_url: None,
                },
                asserted_url: None,
            },
            Step::Press {
                modifiers: vec!["Control".into(), "Shift".into()],
                key: "A".into(),
                scope: Scope {
                    target: "p1".into(),
                    frame: Vec::new(),
                    page_url: None,
                },
                asserted_url: Some("https://example.com/done".into()),
            },
            Step::Wheel {
                target: Some(target(SelectorKind::Css {
                    value: "#scroll".into(),
                })),
                x: 3.0,
                y: 9.0,
                position: None,
                scope: Scope {
                    target: "p1".into(),
                    frame: Vec::new(),
                    page_url: None,
                },
            },
            Step::Upload {
                target: target(SelectorKind::Css {
                    value: "input[type=file]".into(),
                }),
                paths: vec!["one.txt".into(), "two.txt".into()],
                scope: Scope {
                    target: "p1".into(),
                    frame: Vec::new(),
                    page_url: None,
                },
            },
            Step::OpenPage {
                url: "https://example.com/same".into(),
                source_scope: Scope {
                    target: "p1".into(),
                    frame: Vec::new(),
                    page_url: None,
                },
                scope: Scope {
                    target: "p2".into(),
                    frame: Vec::new(),
                    page_url: None,
                },
            },
            Step::NewPage {
                url: Some("https://example.com/same".into()),
                scope: Scope {
                    target: "p3".into(),
                    frame: Vec::new(),
                    page_url: None,
                },
            },
            // A snapshot ref: the primary capture path, and the only one that
            // produces a role locator.
            Step::Hover {
                target: Target {
                    selectors: vec![
                        SelectorKind::Role {
                            role: "button".into(),
                            name: "Save \"now\"".into(),
                            nth: None,
                        },
                        SelectorKind::Css {
                            value: "#save".into(),
                        },
                    ],
                    input_type: None,
                    verified: true,
                },
                scope: Scope {
                    target: "p1".into(),
                    frame: Vec::new(),
                    page_url: None,
                },
            },
            Step::ClosePage {
                scope: Scope {
                    target: "p2".into(),
                    frame: Vec::new(),
                    page_url: None,
                },
            },
        ];
        assert_eq!(
            render_playwright("hostile \"flow\"\nname", &steps).trim_end(),
            include_str!("test-fixtures/flow.spec.ts").trim_end()
        );
    }

    #[test]
    fn an_unverified_target_uses_the_first_match_and_warns() {
        let unverified = Target {
            selectors: vec![SelectorKind::Css {
                value: "button".into(),
            }],
            input_type: None,
            verified: false,
        };
        let click = |target: Target| Step::Pointer {
            target,
            kind: ClickKind::Click,
            pointer: PointerKind::Mouse,
            button: "left".into(),
            count: 1,
            position: None,
            opens_popup: false,
            popup_page: None,
            scope: Scope::default(),
            asserted_url: None,
        };

        // The command acted on the first match, and Playwright refuses a
        // locator that matches several elements.
        let rendered = render_playwright_with_report("first", &[click(unverified)]);
        assert!(
            rendered.output.contains("locator(\"button\").first()"),
            "{}",
            rendered.output
        );
        assert!(
            rendered
                .issues
                .iter()
                .any(|issue| issue.code == "playwright-target-not-unique" && !issue.omitted),
            "the fallback must be reported: {:?}",
            rendered.issues
        );

        // A probed selector is verified, so it addresses one element as it is.
        let verified = Target {
            selectors: vec![SelectorKind::Css {
                value: "#save".into(),
            }],
            input_type: None,
            verified: true,
        };
        let rendered = render_playwright_with_report("verified", &[click(verified)]);
        assert!(
            rendered.output.contains("locator(\"#save\").click"),
            "{}",
            rendered.output
        );
        assert!(!rendered.output.contains(".first()"), "{}", rendered.output);
        assert!(rendered.issues.is_empty(), "{:?}", rendered.issues);
    }

    #[test]
    fn a_count_assertion_keeps_every_match() {
        let steps = vec![Step::WaitForElement {
            target: Target {
                selectors: vec![SelectorKind::Css { value: "li".into() }],
                input_type: None,
                verified: false,
            },
            scope: Scope::default(),
            visible: None,
            properties: Default::default(),
            count: Some(3),
            operator: None,
        }];

        let rendered = render_playwright_with_report("count", &steps);

        assert!(
            !rendered.output.contains(".first()"),
            "a count assertion addresses every match: {}",
            rendered.output
        );
        assert!(rendered.issues.is_empty(), "{:?}", rendered.issues);
    }

    #[test]
    fn role_locators_use_exact_accessible_names() {
        let steps = vec![Step::Pointer {
            target: target(SelectorKind::Role {
                role: "button".into(),
                name: "Save".into(),
                nth: None,
            }),
            kind: ClickKind::Click,
            pointer: PointerKind::Mouse,
            button: "left".into(),
            count: 1,
            position: None,
            opens_popup: false,
            popup_page: None,
            scope: Scope::default(),
            asserted_url: None,
        }];

        let rendered = render_playwright("roles", &steps);

        // Capture resolved the ref by exact role and name, so a page with a
        // `Save draft` button must not match here.
        assert!(
            rendered.contains("getByRole(\"button\", { name: \"Save\", exact: true })"),
            "{rendered}"
        );
    }

    #[test]
    fn a_resize_back_to_the_initial_viewport_stays_in_the_test() {
        let viewport = |width: i64, height: i64| Step::ScopedViewport {
            width,
            height,
            device_scale_factor: 1.0,
            is_mobile: false,
            scope: Scope::default(),
        };
        let steps = vec![viewport(1280, 720), viewport(800, 600), viewport(1280, 720)];

        let rendered = render_playwright("viewports", &steps);

        // The first step is the context setup that `test.use` already carries.
        assert_eq!(
            rendered.matches("setViewportSize").count(),
            2,
            "the resize and the return must both stay: {rendered}"
        );
        assert!(
            rendered.contains("setViewportSize({ width: 800, height: 600 })"),
            "{rendered}"
        );
        assert!(
            rendered.contains("setViewportSize({ width: 1280, height: 720 })"),
            "{rendered}"
        );
    }
}
