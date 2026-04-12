use serde_json::Value;

use super::cdp::client::CdpClient;
use super::cdp::types::{EvaluateParams, EvaluateResult};

/// Query all running animations via the Web Animations API.
/// Returns structured JSON with animation details.
pub async fn list_animations(client: &CdpClient, session_id: &str) -> Result<Value, String> {
    let expression = r#"(() => {
        const animations = document.getAnimations();
        return animations.map((a, i) => {
            const effect = a.effect;
            const target = effect && effect.target;
            const timing = effect && effect.getTiming ? effect.getTiming() : null;
            const computed = effect && effect.getComputedTiming ? effect.getComputedTiming() : null;
            let keyframes = null;
            try { keyframes = effect && effect.getKeyframes ? effect.getKeyframes() : null; } catch(e) {}

            let targetDesc = null;
            if (target) {
                const tag = target.tagName ? target.tagName.toLowerCase() : '';
                const id = target.id ? '#' + target.id : '';
                const cls = target.className && typeof target.className === 'string'
                    ? '.' + target.className.trim().split(/\s+/).join('.')
                    : '';
                targetDesc = tag + id + cls;
            }

            return {
                index: i,
                id: a.id || null,
                animationName: a.animationName || null,
                playState: a.playState,
                currentTime: a.currentTime,
                startTime: a.startTime,
                playbackRate: a.playbackRate,
                target: targetDesc,
                duration: timing ? timing.duration : null,
                delay: timing ? timing.delay : null,
                endDelay: timing ? timing.endDelay : null,
                iterations: timing ? timing.iterations : null,
                direction: timing ? timing.direction : null,
                easing: timing ? timing.easing : null,
                fill: timing ? timing.fill : null,
                progress: computed ? computed.progress : null,
                activeDuration: computed ? computed.activeDuration : null,
                localTime: computed ? computed.localTime : null,
                keyframes: keyframes,
                type: a.constructor.name
            };
        });
    })()"#;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: expression.to_string(),
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;

    if let Some(details) = &result.exception_details {
        let text = details
            .exception
            .as_ref()
            .and_then(|e| e.description.as_deref())
            .unwrap_or(&details.text);
        return Err(format!("Failed to list animations: {}", text));
    }

    match result.result.value {
        Some(v) => Ok(v),
        None => Ok(serde_json::json!([]))
    }
}

/// Pause all animations or a specific animation by index.
pub async fn pause_animations(
    client: &CdpClient,
    session_id: &str,
    index: Option<u32>,
) -> Result<Value, String> {
    let expression = match index {
        Some(i) => format!(
            r#"(() => {{
                const anims = document.getAnimations();
                if ({i} >= anims.length) return {{ error: 'Index {i} out of range, ' + anims.length + ' animations found' }};
                anims[{i}].pause();
                return {{ paused: 1, index: {i} }};
            }})()"#,
            i = i
        ),
        None => r#"(() => {
            const anims = document.getAnimations();
            anims.forEach(a => a.pause());
            return { paused: anims.length };
        })()"#
            .to_string(),
    };

    eval_and_return(client, session_id, &expression).await
}

/// Resume all animations or a specific animation by index.
pub async fn resume_animations(
    client: &CdpClient,
    session_id: &str,
    index: Option<u32>,
) -> Result<Value, String> {
    let expression = match index {
        Some(i) => format!(
            r#"(() => {{
                const anims = document.getAnimations();
                if ({i} >= anims.length) return {{ error: 'Index {i} out of range, ' + anims.length + ' animations found' }};
                anims[{i}].play();
                return {{ resumed: 1, index: {i} }};
            }})()"#,
            i = i
        ),
        None => r#"(() => {
            const anims = document.getAnimations();
            anims.forEach(a => a.play());
            return { resumed: anims.length };
        })()"#
            .to_string(),
    };

    eval_and_return(client, session_id, &expression).await
}

/// Scrub all animations or a specific one to a given progress (0.0–1.0).
pub async fn scrub_animations(
    client: &CdpClient,
    session_id: &str,
    progress: f64,
    index: Option<u32>,
) -> Result<Value, String> {
    let progress = progress.clamp(0.0, 1.0);

    let expression = match index {
        Some(i) => format!(
            r#"(() => {{
                const anims = document.getAnimations();
                if ({i} >= anims.length) return {{ error: 'Index {i} out of range, ' + anims.length + ' animations found' }};
                const a = anims[{i}];
                a.pause();
                const timing = a.effect && a.effect.getComputedTiming ? a.effect.getComputedTiming() : null;
                const duration = timing ? timing.activeDuration : (a.effect && a.effect.getTiming ? a.effect.getTiming().duration : 0);
                a.currentTime = duration * {progress};
                return {{ scrubbed: 1, index: {i}, progress: {progress}, currentTime: a.currentTime }};
            }})()"#,
            i = i,
            progress = progress
        ),
        None => format!(
            r#"(() => {{
                const anims = document.getAnimations();
                let count = 0;
                anims.forEach(a => {{
                    a.pause();
                    const timing = a.effect && a.effect.getComputedTiming ? a.effect.getComputedTiming() : null;
                    const duration = timing ? timing.activeDuration : (a.effect && a.effect.getTiming ? a.effect.getTiming().duration : 0);
                    a.currentTime = duration * {progress};
                    count++;
                }});
                return {{ scrubbed: count, progress: {progress} }};
            }})()"#,
            progress = progress
        ),
    };

    eval_and_return(client, session_id, &expression).await
}

/// Audit animations for performance and a11y issues.
/// Checks for: animating layout properties, missing prefers-reduced-motion,
/// excessive duration, infinite iterations without purpose.
pub async fn audit_animations(client: &CdpClient, session_id: &str) -> Result<Value, String> {
    let expression = r#"(() => {
        const LAYOUT_PROPS = new Set([
            'width', 'height', 'top', 'left', 'right', 'bottom',
            'margin', 'margin-top', 'margin-right', 'margin-bottom', 'margin-left',
            'padding', 'padding-top', 'padding-right', 'padding-bottom', 'padding-left',
            'border-width', 'border-top-width', 'border-right-width', 'border-bottom-width', 'border-left-width',
            'font-size', 'line-height'
        ]);
        const PERF_GOOD = new Set(['transform', 'opacity', 'filter', 'clip-path']);

        const animations = document.getAnimations();
        const results = [];

        // Check prefers-reduced-motion
        const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
        let reducedMotionStylesheet = false;
        try {
            for (const sheet of document.styleSheets) {
                try {
                    for (const rule of sheet.cssRules) {
                        if (rule.conditionText && rule.conditionText.includes('prefers-reduced-motion')) {
                            reducedMotionStylesheet = true;
                            break;
                        }
                    }
                } catch(e) { /* cross-origin */ }
                if (reducedMotionStylesheet) break;
            }
        } catch(e) {}

        for (const anim of animations) {
            const issues = [];
            const effect = anim.effect;
            const timing = effect && effect.getTiming ? effect.getTiming() : null;

            // Check animated properties for layout triggers
            let keyframes = [];
            try { keyframes = effect && effect.getKeyframes ? effect.getKeyframes() : []; } catch(e) {}

            const animatedProps = new Set();
            for (const kf of keyframes) {
                for (const key of Object.keys(kf)) {
                    if (key !== 'offset' && key !== 'easing' && key !== 'composite' && key !== 'computedOffset') {
                        animatedProps.add(key);
                    }
                }
            }

            const layoutProps = [];
            const perfGoodProps = [];
            for (const prop of animatedProps) {
                const cssProp = prop.replace(/([A-Z])/g, '-$1').toLowerCase();
                if (LAYOUT_PROPS.has(cssProp)) layoutProps.push(cssProp);
                if (PERF_GOOD.has(cssProp)) perfGoodProps.push(cssProp);
            }

            if (layoutProps.length > 0) {
                issues.push({
                    severity: 'warning',
                    type: 'layout-trigger',
                    message: 'Animating layout properties causes reflow: ' + layoutProps.join(', '),
                    suggestion: 'Use transform/opacity instead for smooth 60fps animations'
                });
            }

            // Duration checks
            if (timing && timing.duration > 5000) {
                issues.push({
                    severity: 'info',
                    type: 'long-duration',
                    message: 'Animation duration is ' + timing.duration + 'ms (>5s)',
                    suggestion: 'Consider shorter duration for better UX'
                });
            }

            // Infinite iteration check
            if (timing && timing.iterations === Infinity && !timing.fill) {
                issues.push({
                    severity: 'info',
                    type: 'infinite-no-fill',
                    message: 'Infinite animation with no fill mode'
                });
            }

            const target = effect && effect.target;
            let targetDesc = null;
            if (target) {
                const tag = target.tagName ? target.tagName.toLowerCase() : '';
                const id = target.id ? '#' + target.id : '';
                targetDesc = tag + id;
            }

            results.push({
                animationName: anim.animationName || anim.id || null,
                type: anim.constructor.name,
                target: targetDesc,
                playState: anim.playState,
                duration: timing ? timing.duration : null,
                iterations: timing ? timing.iterations : null,
                easing: timing ? timing.easing : null,
                animatedProperties: Array.from(animatedProps),
                performanceGood: layoutProps.length === 0 && perfGoodProps.length > 0,
                issues: issues
            });
        }

        return {
            totalAnimations: animations.length,
            prefersReducedMotionActive: reducedMotion,
            prefersReducedMotionHandled: reducedMotionStylesheet,
            a11yWarning: (!reducedMotionStylesheet && animations.length > 0)
                ? 'No prefers-reduced-motion media query found — users who prefer reduced motion will still see all animations'
                : null,
            animations: results
        };
    })()"#;

    eval_and_return(client, session_id, expression).await
}

async fn eval_and_return(
    client: &CdpClient,
    session_id: &str,
    expression: &str,
) -> Result<Value, String> {
    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: expression.to_string(),
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;

    if let Some(details) = &result.exception_details {
        let text = details
            .exception
            .as_ref()
            .and_then(|e| e.description.as_deref())
            .unwrap_or(&details.text);
        return Err(format!("JS evaluation error: {}", text));
    }

    match result.result.value {
        Some(v) => Ok(v),
        None => Ok(serde_json::json!(null)),
    }
}
