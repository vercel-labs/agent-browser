/**
 * Standalone CLI entry point for auth vault operations that don't need a browser.
 * Invoked directly by the Rust CLI to avoid sending passwords through the daemon channel.
 *
 * Usage: node auth-cli.js <json-command>
 * Prints a JSON response to stdout and exits.
 */
import {
  saveAuthProfile,
  getAuthProfileMeta,
  listAuthProfiles,
  deleteAuthProfile,
} from './auth-vault.js';

interface AuthCommand {
  id: string;
  action: string;
  name?: string;
  url?: string;
  username?: string;
  password?: string;
  usernameSelector?: string;
  passwordSelector?: string;
  submitSelector?: string;
}

function success(id: string, data: Record<string, unknown>): string {
  return JSON.stringify({ success: true, id, data });
}

function error(id: string, message: string): string {
  return JSON.stringify({ success: false, id, error: message });
}

function run(): void {
  const input = process.argv[2];
  if (!input) {
    process.stderr.write('Usage: node auth-cli.js <json-command>\n');
    process.exit(1);
  }

  let cmd: AuthCommand;
  try {
    cmd = JSON.parse(input);
  } catch {
    console.log(error('', 'Invalid JSON input'));
    process.exit(1);
    return;
  }

  const id = cmd.id || '';

  try {
    switch (cmd.action) {
      case 'auth_save': {
        if (!cmd.name || !cmd.url || !cmd.username || !cmd.password) {
          console.log(error(id, 'Missing required fields: name, url, username, password'));
          return;
        }
        const meta = saveAuthProfile({
          name: cmd.name,
          url: cmd.url,
          username: cmd.username,
          password: cmd.password,
          usernameSelector: cmd.usernameSelector,
          passwordSelector: cmd.passwordSelector,
          submitSelector: cmd.submitSelector,
        });
        console.log(
          success(id, {
            saved: !meta.updated,
            updated: meta.updated,
            name: meta.name,
            url: meta.url,
            username: meta.username,
          })
        );
        return;
      }
      case 'auth_list': {
        const profiles = listAuthProfiles();
        console.log(success(id, { profiles }));
        return;
      }
      case 'auth_show': {
        if (!cmd.name) {
          console.log(error(id, 'Missing required field: name'));
          return;
        }
        const meta = getAuthProfileMeta(cmd.name);
        if (!meta) {
          console.log(error(id, `Auth profile '${cmd.name}' not found`));
          return;
        }
        console.log(success(id, { profile: meta }));
        return;
      }
      case 'auth_delete': {
        if (!cmd.name) {
          console.log(error(id, 'Missing required field: name'));
          return;
        }
        const deleted = deleteAuthProfile(cmd.name);
        if (!deleted) {
          console.log(error(id, `Auth profile '${cmd.name}' not found`));
          return;
        }
        console.log(success(id, { deleted: true, name: cmd.name }));
        return;
      }
      default:
        console.log(error(id, `Unknown auth action: ${cmd.action}`));
    }
  } catch (err) {
    const msg = err instanceof Error ? err.message : 'Operation failed';
    console.log(error(id, msg));
  }
}

run();
