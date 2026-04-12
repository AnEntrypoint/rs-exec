## 2026-04-12
- managed_browser_user_data: use cwd/.plugkit-browser-profile instead of %LOCALAPPDATA%/plugkit/browser-profiles/<session_id>
- Auto-add .plugkit-browser-profile to .gitignore on first browser launch
- Remove browser kill from idle session cleanup — browser survives idle timeout, only tasks cleaned
