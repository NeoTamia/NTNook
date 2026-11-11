#!/usr/bin/env python3
import http.server,json,os,pathlib,sys,time
args=sys.argv
if '--help' in args:
 print('--name --url --config --traffic-policy-file --upstream-tls-verify --pooling-enabled');sys.exit(0)
if args[1:3]==['config','check']: sys.exit(0)
config=pathlib.Path(args[args.index('--config')+1].split(',')[-1])
root=config.parent
(root/'argv.json').write_text(json.dumps(args))
mode=os.environ.get('NOOK_FAKE_MODE','ok')
if mode=='bind-retry' and not (root/'retried').exists():
 (root/'retried').touch();print('address already in use');sys.exit(1)
if mode=='exit':
 print('secret-value-should-never-print');sys.exit(7)
port=int(config.read_text().split('web_addr: 127.0.0.1:')[1].split()[0])
name=args[args.index('--name')+1]
url=args[args.index('--url')+1] if '--url' in args else 'https://fixture-'+str(os.getpid())+'.ngrok-free.dev'
class Handler(http.server.BaseHTTPRequestHandler):
 def do_GET(self):
  if (root/'exit').exists(): os._exit(9)
  endpoint={'name':name if mode!='foreign' else 'other','url':url,'upstream':{'url':args[2]}}
  if mode=='wrong-upstream': endpoint['upstream']['url']='http://127.0.0.1:1'
  if mode=='wrong-url': endpoint['url']='https://evil.example'
  body=b'not-json' if mode=='malformed' else json.dumps({'endpoints':[endpoint]}).encode()
  self.send_response(503 if mode=='timeout' or (root/'offline').exists() else 200)
  self.end_headers();self.wfile.write(body)
 def log_message(self,*a): pass
server=http.server.HTTPServer(('127.0.0.1',port),Handler)
while mode=='delayed' and not (root/'release').exists(): time.sleep(0.02)
server.serve_forever()
