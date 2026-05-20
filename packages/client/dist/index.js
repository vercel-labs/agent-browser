const CURSOR_KEYWORDS = [
    "auto",
    "default",
    "none",
    "context-menu",
    "help",
    "pointer",
    "progress",
    "wait",
    "cell",
    "crosshair",
    "text",
    "vertical-text",
    "alias",
    "copy",
    "move",
    "no-drop",
    "not-allowed",
    "grab",
    "grabbing",
    "all-scroll",
    "col-resize",
    "row-resize",
    "n-resize",
    "e-resize",
    "s-resize",
    "w-resize",
    "ne-resize",
    "nw-resize",
    "se-resize",
    "sw-resize",
    "ew-resize",
    "ns-resize",
    "nesw-resize",
    "nwse-resize",
    "zoom-in",
    "zoom-out",
];
export function sanitizeCursorKeyword(cursor) {
    if (typeof cursor !== "string")
        return "default";
    const keyword = cursor
        .split(",")
        .reverse()
        .map((part) => part.trim().toLowerCase())
        .find((part) => CURSOR_KEYWORDS.includes(part));
    return keyword ?? "default";
}
export function cdpButton(button) {
    switch (button) {
        case 0:
            return "left";
        case 1:
            return "middle";
        case 2:
            return "right";
        default:
            return "none";
    }
}
export function cdpButtons(buttons) {
    let mask = 0;
    if (buttons & 1)
        mask |= 1;
    if (buttons & 2)
        mask |= 2;
    if (buttons & 4)
        mask |= 4;
    if (buttons & 8)
        mask |= 8;
    if (buttons & 16)
        mask |= 16;
    return mask;
}
export function cdpButtonsForEvent(eventType, button, buttons) {
    const mask = cdpButtons(buttons);
    if (eventType !== "mousePressed" || mask !== 0)
        return mask;
    switch (button) {
        case 0:
            return 1;
        case 1:
            return 4;
        case 2:
            return 2;
        default:
            return 0;
    }
}
