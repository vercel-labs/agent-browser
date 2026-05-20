export interface FrameMessage {
    type: "frame";
    data: string;
    metadata: {
        offsetTop: number;
        pageScaleFactor: number;
        deviceWidth: number;
        deviceHeight: number;
        scrollOffsetX: number;
        scrollOffsetY: number;
        timestamp: number;
    };
}
export interface StatusMessage {
    type: "status";
    connected: boolean;
    screencasting: boolean;
    viewportWidth: number;
    viewportHeight: number;
    engine?: string;
    recording?: boolean;
}
export interface CommandMessage {
    type: "command";
    action: string;
    id: string;
    params: Record<string, unknown>;
    timestamp: number;
}
export interface ResultMessage {
    type: "result";
    id: string;
    action: string;
    success: boolean;
    data: unknown;
    duration_ms: number;
    timestamp: number;
}
export interface ConsoleMessage {
    type: "console";
    level: string;
    text: string;
    timestamp: number;
}
export interface UrlMessage {
    type: "url";
    url: string;
    timestamp: number;
}
export interface PageErrorMessage {
    type: "page_error";
    text: string;
    line: number | null;
    column: number | null;
    timestamp: number;
}
export interface ErrorMessage {
    type: "error";
    message: string;
}
export interface CursorMessage {
    type: "cursor";
    cursor: CursorKeyword;
    x: number;
    y: number;
    timestamp: number;
}
export interface TabInfo {
    tabId: string;
    label?: string | null;
    title: string;
    url: string;
    type: string;
    active: boolean;
}
export interface TabsMessage {
    type: "tabs";
    tabs: TabInfo[];
    timestamp: number;
}
export type StreamMessage = FrameMessage | StatusMessage | CommandMessage | ResultMessage | ConsoleMessage | PageErrorMessage | ErrorMessage | CursorMessage | UrlMessage | TabsMessage;
export type ActivityEvent = CommandMessage | ResultMessage | ConsoleMessage;
export type ConsoleEntry = ConsoleMessage | PageErrorMessage;
declare const CURSOR_KEYWORDS: readonly ["auto", "default", "none", "context-menu", "help", "pointer", "progress", "wait", "cell", "crosshair", "text", "vertical-text", "alias", "copy", "move", "no-drop", "not-allowed", "grab", "grabbing", "all-scroll", "col-resize", "row-resize", "n-resize", "e-resize", "s-resize", "w-resize", "ne-resize", "nw-resize", "se-resize", "sw-resize", "ew-resize", "ns-resize", "nesw-resize", "nwse-resize", "zoom-in", "zoom-out"];
export type CursorKeyword = (typeof CURSOR_KEYWORDS)[number];
export type CdpMouseButton = "left" | "middle" | "right" | "none";
export declare function sanitizeCursorKeyword(cursor: unknown): CursorKeyword;
export declare function cdpButton(button: number): CdpMouseButton;
export declare function cdpButtons(buttons: number): number;
export declare function cdpButtonsForEvent(eventType: string, button: number, buttons: number): number;
export {};
//# sourceMappingURL=index.d.ts.map