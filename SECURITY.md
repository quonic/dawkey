# Security

## Reporting a vulnerability

Please report security problems privately, through GitHub's private vulnerability reporting: open the repository's **Security** tab and choose **Report a vulnerability**. Don't open a public issue for them.

Say what the problem is, how to reproduce it, and which version or commit you tested. You'll get an answer as soon as the maintainer can look at it, and credit in the fix unless you'd rather not.

Dawkey is in early development, so fixes go into `main` and the next release. Older versions don't get separate fixes.

## What Dawkey protects, and how

Dawkey runs with your permissions. These are the parts of it that other programs or files can reach, and what keeps them in bounds.

### MCP

An MCP client controls Dawkey completely: it can edit and save the project, import audio from any path you can read, write exports and projects anywhere you can write, load plugins, and click anything in the window. Connect only clients you trust as much as you trust yourself at the keyboard.

- **The window's Unix socket** is `$XDG_RUNTIME_DIR/dawkey/<pid>.sock`, in a folder only your user can open. Other users on the machine can't connect.
- **Headless instances** (`dawkey mcp --headless`) talk only to the program that started them, over stdin and stdout.
- **MCP over HTTP** is off unless you turn it on in the preferences (or run `dawkey mcp --headless --http`). It listens only on loopback addresses. Every request needs `Authorization: Bearer <token>`. The token is kept in `$XDG_CONFIG_HOME/dawkey/mcp-token`, readable only by you, or comes from `$DAWKEY_MCP_TOKEN` for headless instances. Requests whose `Host` or `Origin` header isn't local are refused, so a web page in your browser can't reach it. "New token" in the preferences replaces the token.

A way to reach any of these without the access described above (another user reaching the socket, a request getting through without the token, a web page reaching the HTTP server) is a vulnerability.

### Plugins

Plugins are native code and can do anything your user can. In the window, Dawkey runs each plugin file in a process of its own (`dawkey plugin-host`), and by default that process enters a sandbox before it loads the plugin:

- Landlock lets it write only to the XDG config, cache, data and state folders, `~/.lv2`, `~/.vst3/presets`, `/tmp`, `/var/tmp`, `/dev`, `/proc/self` and Dawkey's folder for plugin files. It can still read everywhere, because samplers load files from anywhere.
- It can't open TCP connections or internet sockets.
- seccomp stops it from tracing other processes or reading their memory, loading kernel modules, mounting, making namespaces, and using BPF or io_uring.

The sandbox is meant to contain a plugin that misbehaves, not to make an untrusted plugin safe to run. Install plugins only from sources you trust.

Headless instances run plugins inside Dawkey's own process unless they're given `--bridge-plugins`, and then without the sandbox only if given `--no-sandbox` too. "Sandbox" and "Own processes" in the preferences turn these off for the window.

A plugin breaking out of the sandbox, or a project file that makes Dawkey run code or write outside the project, is a vulnerability.

### Project files

Opening a project loads the plugins it names (from those installed) and gives them the state saved with them. A project can't contain code of its own. Treat projects from other people like any document that drives installed software: a plugin with a bug may still mishandle the state it's given.
