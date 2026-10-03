#!/usr/bin/env node
// Explicit native-login fixture. Never authenticates or calls a model.
const fs=require('node:fs');
const args=process.argv.slice(2);
if(!process.stdin.isTTY||!process.stdout.isTTY||process.env.HOME!=='/config')process.exit(2);
if(args[0]==='login') {
  if(args.join(' ')!=='login --oauth --device-auth')process.exit(3);
} else if(!['--no-tools','--no-builtin-tools','--no-extensions','--no-skills','--no-context-files'].every(arg=>args.includes(arg)))process.exit(4);
if(fs.existsSync('/config/fixture-wait')) {
  process.stdout.write('FIXTURE_LOGIN_WAITING\n');
  process.on('SIGINT',()=>process.exit(130));
  setInterval(()=>{},1000);
} else {
  fs.writeFileSync('/config/auth.json',JSON.stringify({fixture:'synthetic-login-material'}),{mode:0o600});
  process.stdout.write('FIXTURE_LOGIN_SAVED\n');
}
