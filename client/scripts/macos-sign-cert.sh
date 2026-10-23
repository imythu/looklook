#!/usr/bin/env bash
# 生成 macOS 发布用的自签名代码签名证书（只需要做一次，之后每个版本都用它签）。
#
#   scripts/macos-sign-cert.sh [输出目录]
#
# 为什么要：macOS 按代码签名记“允许看看访问文稿 / 桌面 / 下载文件夹”这类授权。ad-hoc 签名每个版本都不一样，
# 系统把升级后的看看当成另一个程序，每次升级都会重新询问。每个版本都用同一张证书签，授权就一直有效。
# 自签名证书不能让 Gatekeeper 放行（第一次打开仍要在“隐私与安全性”里允许，和 ad-hoc 一样），只解决重复询问。
#
# 生成后把输出的两项加到公开仓库（imythu/looklook）的 Actions secrets：
#   MACOS_SIGN_P12           looklook-sign.p12.b64 的内容（base64）
#   MACOS_SIGN_P12_PASSWORD  脚本打印的密码
# client-release 工作流的 macOS 任务会导入它，打包时经 LOOKLOOK_MACOS_SIGN_IDENTITY 交给 scripts/package.sh。
# 证书和私钥请妥善保管：换了证书，用户升级后会再被询问一次。有效期 20 年。
set -euo pipefail

out=${1:-.}
mkdir -p "$out"
name="Looklook Code Signing"
pass=$(openssl rand -hex 16)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

cat >"$tmp/cert.cnf" <<EOF
[req]
distinguished_name = dn
prompt = no
x509_extensions = ext
[dn]
CN = $name
O = Looklook
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
subjectKeyIdentifier = hash
EOF

openssl req -x509 -newkey rsa:2048 -sha256 -days 7300 -nodes -keyout "$tmp/key.pem" -out "$tmp/cert.pem" -config "$tmp/cert.cnf"
# macOS 的 security import 认旧的 PKCS#12 加密方式；OpenSSL 3 要加 -legacy
legacy=()
openssl pkcs12 -help 2>&1 | grep -q -- '-legacy' && legacy=(-legacy)
openssl pkcs12 -export "${legacy[@]}" -inkey "$tmp/key.pem" -in "$tmp/cert.pem" -name "$name" -out "$out/looklook-sign.p12" -passout "pass:$pass"
base64 <"$out/looklook-sign.p12" | tr -d '\n' >"$out/looklook-sign.p12.b64"
chmod 600 "$out/looklook-sign.p12" "$out/looklook-sign.p12.b64"

echo "证书：$name（SHA-1 $(openssl x509 -in "$tmp/cert.pem" -noout -fingerprint -sha1 | cut -d= -f2 | tr -d :)）"
echo "已生成：$out/looklook-sign.p12 与 $out/looklook-sign.p12.b64"
echo "MACOS_SIGN_P12_PASSWORD=$pass"
echo "把 .b64 文件的内容和上面的密码加到公开仓库的 Actions secrets（MACOS_SIGN_P12、MACOS_SIGN_P12_PASSWORD）。"
