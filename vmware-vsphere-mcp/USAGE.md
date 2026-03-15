# VMware vSphere MCP — 使用說明

透過 [giuliolibrando/vmware-vsphere-mcp-server](https://github.com/giuliolibrando/vmware-vsphere-mcp-server) 將 VMware vCenter 整合到 Claude Desktop。

---

## 前置需求

- Docker & Docker Compose
- VMware vCenter Server（有 REST API 存取權限）
- Claude Desktop

---

## 安裝步驟

### 1. 進入設定目錄

```bash
cd vmware-vsphere-mcp
```

### 2. 建立 `.env` 憑證設定

```bash
cp .env.example .env
```

編輯 `.env`，填入你的 vCenter 資訊：

```env
VCENTER_HOST=vcenter.your-domain.local      # vCenter 主機名稱或 IP
VCENTER_USER=administrator@vsphere.local    # 帳號
VCENTER_PASSWORD=your_password_here         # 密碼
INSECURE=True                               # 自簽憑證時設為 True
```

### 3. 啟動 MCP Server

```bash
docker compose up -d --build
```

首次執行會從 GitHub clone 原始碼並建置映像，需要幾分鐘。

確認是否啟動成功：

```bash
docker compose logs -f
# 或
curl http://localhost:8000/health
```

### 4. 設定 Claude Desktop

將以下內容加入 Claude Desktop 的設定檔：

| 作業系統 | 設定檔路徑 |
|---------|-----------|
| macOS   | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Linux   | `~/.config/Claude/claude_desktop_config.json` |
| Windows | `%APPDATA%\Claude\claude_desktop_config.json` |

```json
{
  "mcpServers": {
    "vsphere": {
      "type": "streamable-http",
      "url": "http://localhost:8000/mcp"
    }
  }
}
```

> 若設定檔已有其他 MCP server，只需把 `"vsphere": { ... }` 加入 `mcpServers` 物件即可。

### 5. 重新啟動 Claude Desktop

儲存設定後重啟，Claude 即可使用 vSphere 工具。

---

## 自動安裝（選用）

```bash
./setup.sh
```

腳本會自動：
1. 從 `.env.example` 建立 `.env`（若不存在則提示填寫後重跑）
2. 執行 `docker compose up -d --build`
3. 寫入 Claude Desktop 設定檔

---

## 可用工具

啟動後，Claude 可執行以下 vSphere 操作：

| 類別 | 工具 |
|------|------|
| **VM 查詢** | 列出所有 VM 及電源狀態、取得 VM 詳細資訊 |
| **VM 控制** | 開機、關機、重啟、暫停 |
| **快照管理** | 建立快照、刪除快照（需確認） |
| **資源監控** | CPU、記憶體、儲存使用率 |
| **基礎設施** | 列出 Hosts、Datastores、Networks、Datacenters |
| **事件與警報** | 查詢事件日誌、列出現有警報 |
| **報表** | 產生環境資源使用摘要 |

> **破壞性操作**（刪除 VM、修改資源）需明確傳入 `confirm=True` 才會執行，防止誤操作。

### 使用範例

在 Claude Desktop 中直接用自然語言問：

```
列出所有正在運行的虛擬機器
```
```
幫我對 VM "web-server-01" 建立快照，名稱叫 "before-upgrade"
```
```
顯示過去 24 小時 CPU 使用率最高的前 5 台 VM
```
```
產生整個 vSphere 環境的資源使用報表
```

---

## 日常管理

```bash
# 停止 MCP server
docker compose down

# 重新啟動
docker compose up -d

# 查看 logs
docker compose logs -f

# 更新到最新版
docker compose pull && docker compose up -d
```

---

## 疑難排解

**無法連線 vCenter**
- 確認 `VCENTER_HOST` 可以從 Docker 容器內 ping 到
- 自簽憑證環境確認 `INSECURE=True`

**Claude Desktop 看不到 vsphere 工具**
- 確認 `docker compose ps` 顯示 container 狀態為 `Up`
- 確認設定檔格式正確（無多餘逗號）
- 重新啟動 Claude Desktop

**Port 8000 衝突**
- 修改 `docker-compose.yml` 的 `ports` 改為其他 port（如 `8001:8000`）
- 同步更新 `claude_desktop_config.json` 的 URL
