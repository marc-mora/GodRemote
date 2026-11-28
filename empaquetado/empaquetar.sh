#!/usr/bin/env bash
# Prepara los paquetes de una versión de GodRemote a partir de los
# ejecutables ya compiladas las dos ediciones.
#
#   empaquetado/empaquetar.sh <linux|macos|windows> <carpeta-binarios> <salida>
#
# <carpeta-binarios> contiene `godremote` y `godremote-reforzado` (con .exe en
# Windows; en macOS, binarios universales). Lo usa el CI, y sirve igual a mano.
set -euo pipefail

sistema="$1"
binarios="$(cd "$2" && pwd)"
mkdir -p "$3"
salida="$(cd "$3" && pwd)"
raiz="$(cd "$(dirname "$0")/.." && pwd)"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$raiz/Cargo.toml" | head -1)"
trabajo="$(mktemp -d)"
trap 'rm -rf "$trabajo"' EXIT

# Edición: nombre del binario, nombre visible, identificador y resumen.
ediciones=(
  "godremote|GodRemote|godremote|Gestor de conexiones remotas (SSH, SFTP, RDP, VNC)"
  "godremote-reforzado|GodRemote-ssh-reforzado|godremote-reforzado|Gestor de conexiones remotas, edición SSH reforzada (sin RDP integrado)"
)

linux() {
  local binario="$1" visible="$2" id="$3" resumen="$4"
  # Portátil: el ejecutable y el manual.
  local carpeta="$trabajo/tar-$id"
  mkdir -p "$carpeta"
  install -m 755 "$binarios/$binario" "$carpeta/$visible"
  install -m 644 "$raiz/README.md" "$carpeta/LEEME.md"
  tar -C "$carpeta" -czf "$salida/$visible-linux-x64.tar.gz" .

  # Paquete .deb, con su entrada en el menú de aplicaciones.
  local deb="$trabajo/deb-$id"
  mkdir -p "$deb/DEBIAN" "$deb/usr/bin" "$deb/usr/share/applications" \
    "$deb/usr/share/icons/hicolor/256x256/apps" "$deb/usr/share/icons/hicolor/512x512/apps" \
    "$deb/usr/share/doc/$id"
  install -m 755 "$binarios/$binario" "$deb/usr/bin/$id"
  install -m 644 "$raiz/assets/icono-256.png" "$deb/usr/share/icons/hicolor/256x256/apps/$id.png"
  install -m 644 "$raiz/assets/icono.png" "$deb/usr/share/icons/hicolor/512x512/apps/$id.png"
  install -m 644 "$raiz/README.md" "$deb/usr/share/doc/$id/LEEME.md"
  sed -e "s|@ID@|$id|g" -e "s|@NOMBRE@|${visible//-/ }|g" -e "s|@RESUMEN@|$resumen|g" \
    "$raiz/empaquetado/linux/aplicacion.desktop" > "$deb/usr/share/applications/$id.desktop"
  local instalado
  instalado="$(du -sk "$deb/usr" | cut -f1)"
  cat > "$deb/DEBIAN/control" <<CONTROL
Package: $id
Version: $version
Section: net
Priority: optional
Architecture: amd64
Installed-Size: $instalado
Depends: libc6 (>= 2.35), libgl1, libx11-6, libxcursor1, libxi6, libxrandr2, libxkbcommon0, libxkbcommon-x11-0
Recommends: xdg-utils
Maintainer: marc-mora <marc-mora@users.noreply.github.com>
Homepage: https://github.com/marc-mora/GodRemote
Description: $resumen
 Árbol de equipos, terminales con pestañas (SSH, Telnet, puerto serie),
 explorador SFTP, escritorios remotos, servidor X propio, túneles y un baúl
 cifrado para las credenciales.
CONTROL
  dpkg-deb --root-owner-group --build "$deb" "$salida/${visible}_${version}_amd64.deb" > /dev/null
}

macos() {
  local binario="$1" visible="$2" id="$3"
  local carpeta="$trabajo/tar-$id"
  mkdir -p "$carpeta"
  install -m 755 "$binarios/$binario" "$carpeta/$visible"
  install -m 644 "$raiz/README.md" "$carpeta/LEEME.md"
  tar -C "$carpeta" -czf "$salida/$visible-macos-universal.tar.gz" .

  # Aplicación (.app) dentro de una imagen de disco.
  local imagen="$trabajo/dmg-$id"
  local app="$imagen/${visible//-/ }.app"
  mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
  install -m 755 "$binarios/$binario" "$app/Contents/MacOS/$visible"
  local iconos="$trabajo/$id.iconset"
  mkdir -p "$iconos"
  local lado
  for lado in 16 32 128 256 512; do
    sips -z "$lado" "$lado" "$raiz/assets/icono-1024.png" --out "$iconos/icon_${lado}x${lado}.png" > /dev/null
    sips -z "$((lado * 2))" "$((lado * 2))" "$raiz/assets/icono-1024.png" \
      --out "$iconos/icon_${lado}x${lado}@2x.png" > /dev/null
  done
  iconutil -c icns "$iconos" -o "$app/Contents/Resources/$visible.icns"
  sed -e "s|@ID@|$id|g" -e "s|@NOMBRE@|${visible//-/ }|g" -e "s|@EJECUTABLE@|$visible|g" \
    -e "s|@VERSION@|$version|g" "$raiz/empaquetado/macos/Info.plist" > "$app/Contents/Info.plist"
  # Firma «ad hoc» (sin identidad): sin ella un Mac con Apple Silicon no lo
  # ejecuta. No sustituye a la firma de desarrollador.
  codesign --force --deep --sign - "$app"
  ln -s /Applications "$imagen/Aplicaciones"
  # A veces falla con «recurso ocupado» en máquinas recién arrancadas.
  local intento
  for intento in 1 2 3; do
    hdiutil create -quiet -volname "${visible//-/ }" -srcfolder "$imagen" -ov -format UDZO \
      "$salida/$visible-macos-universal.dmg" && break
    [ "$intento" -lt 3 ] && sleep 5
  done
  [ -s "$salida/$visible-macos-universal.dmg" ]
}

windows() {
  local binario="$1" visible="$2" id="$3" resumen="$4"
  local carpeta="$trabajo/zip-$id"
  mkdir -p "$carpeta"
  cp "$binarios/$binario.exe" "$carpeta/$visible.exe"
  cp "$raiz/README.md" "$carpeta/LEEME.md"
  (cd "$carpeta" && 7z a -tzip "$salida/$visible-windows-x64.zip" . > /dev/null)

  # Instalador (Inno Setup): por usuario, sin pedir permisos de administrador.
  local iscc="iscc"
  if ! command -v iscc > /dev/null 2>&1; then
    iscc="/c/Program Files (x86)/Inno Setup 6/ISCC.exe"
    # Si el equipo no lo trae, se instala (hace falta Chocolatey).
    [ -x "$iscc" ] || choco install innosetup -y --no-progress > /dev/null
  fi
  # Sin esto, la shell de Git convertiría los «/D…» en rutas.
  MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*' "$iscc" /Qp \
    "/DVersion=$version" "/DNombre=${visible//-/ }" "/DEjecutable=$visible.exe" "/DIdentificador=$id" \
    "/DOrigen=$(cygpath -w "$carpeta")" "/DIcono=$(cygpath -w "$raiz/assets/icono.ico")" \
    "/DSalida=$(cygpath -w "$salida")" "/DFichero=$visible-windows-x64-instalador" \
    "$(cygpath -w "$raiz/empaquetado/windows/instalador.iss")"
}

for edicion in "${ediciones[@]}"; do
  IFS='|' read -r binario visible id resumen <<< "$edicion"
  "$sistema" "$binario" "$visible" "$id" "$resumen"
done
ls -l "$salida"
