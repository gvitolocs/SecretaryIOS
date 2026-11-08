#!/usr/bin/env bash
# Lucina Link: Cloudflare objects (service token, tunnel, Access app, DNS). Prints no secret values.
set -euo pipefail
set -a; . ~/.config/pokoin/cloudflare.env; set +a
ACC=e2620981465c01b9bf805522507d963a
ZONE=b8cf06869f4409950e7d077c2d9a6c41
api(){ local m=$1 p=$2; shift 2; curl -sS -X "$m" -H "X-Auth-Email: $CLOUDFLARE_API_EMAIL" -H "X-Auth-Key: $CLOUDFLARE_API_KEY" -H "Content-Type: application/json" "https://api.cloudflare.com/client/v4$p" "$@"; }
umask 077
mkdir -p ~/.config/lucina-link ~/.config/cloudflared

# 1. Service token (non-expiring)
if [ ! -s ~/.config/lucina-link/secrets.env ]; then
  r=$(api POST /accounts/$ACC/access/service_tokens --data '{"name":"lucina-link","duration":"forever"}')
  echo "$r" | jq -e .success >/dev/null || { echo "$r" | jq -c '.errors'; exit 1; }
  printf 'LUCINA_LINK_CF_CLIENT_ID=%s\nLUCINA_LINK_CF_CLIENT_SECRET=%s\n' "$(echo "$r"|jq -r .result.client_id)" "$(echo "$r"|jq -r .result.client_secret)" > ~/.config/lucina-link/secrets.env
  echo "$r" | jq -r '.result|"service token \(.id) duration=\(.duration) expires=\(.expires_at)"'
  echo "$r" | jq -r .result.id > ~/.config/lucina-link/service-token.id
fi
STID=$(cat ~/.config/lucina-link/service-token.id)

# 2. Tunnel (remote-managed)
TID=$(api GET "/accounts/$ACC/cfd_tunnel?name=lucina-link&is_deleted=false" | jq -r '.result[0].id // empty')
if [ -z "$TID" ]; then
  r=$(api POST /accounts/$ACC/cfd_tunnel --data '{"name":"lucina-link","config_src":"cloudflare"}')
  echo "$r" | jq -e .success >/dev/null || { echo "$r" | jq -c '.errors'; exit 1; }
  TID=$(echo "$r" | jq -r .result.id)
fi
echo "tunnel $TID"
api GET /accounts/$ACC/cfd_tunnel/$TID/token | jq -r .result > ~/.config/cloudflared/lucina-link.token
KINDLE_ORIGIN=${KINDLE_ORIGIN:-kindle.fritz.box:2222}
api PUT /accounts/$ACC/cfd_tunnel/$TID/configurations --data "$(jq -nc --arg k "ssh://$KINDLE_ORIGIN" '{config:{ingress:[
  {hostname:"kindle-ssh.getsecretary.tech",service:$k,originRequest:{connectTimeout:10}},
  {hostname:"nezopt-ssh.getsecretary.tech",service:"ssh://127.0.0.1:22",originRequest:{connectTimeout:10}},
  {service:"http_status:404"}]}}')" | jq -c '{success, ingress: [.result.config.ingress[]?.hostname]}'

# 3. DNS
for h in kindle-ssh nezopt-ssh; do
  id=$(api GET "/zones/$ZONE/dns_records?name=$h.getsecretary.tech" | jq -r '.result[0].id // empty')
  body=$(jq -nc --arg n "$h.getsecretary.tech" --arg c "$TID.cfargotunnel.com" '{type:"CNAME",name:$n,content:$c,proxied:true,comment:"Lucina Link tunnel"}')
  if [ -z "$id" ]; then api POST /zones/$ZONE/dns_records --data "$body"; else api PUT /zones/$ZONE/dns_records/$id --data "$body"; fi | jq -c '{success, name: .result.name, errors}'
done

# 4. Access app + service-token policy
AID=$(api GET /accounts/$ACC/access/apps | jq -r '.result[]|select(.name=="Lucina Link SSH")|.id')
app=$(jq -nc --arg st "$STID" '{name:"Lucina Link SSH",type:"self_hosted",domain:"kindle-ssh.getsecretary.tech",
  self_hosted_domains:["kindle-ssh.getsecretary.tech","nezopt-ssh.getsecretary.tech"],
  session_duration:"24h",app_launcher_visible:false,service_auth_401_redirect:true,
  policies:[{name:"lucina-link service token",decision:"non_identity",include:[{service_token:{token_id:$st}}],precedence:1}]}')
if [ -z "$AID" ]; then api POST /accounts/$ACC/access/apps --data "$app"; else api PUT /accounts/$ACC/access/apps/$AID --data "$app"; fi \
  | jq -c '{success, id: .result.id, domains: .result.self_hosted_domains, policies: [.result.policies[]?.name], errors}'
