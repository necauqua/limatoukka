steam-common := "/storage/games/steam/steamapps/common"
proton := "Proton - Experimental"

export STEAM_COMPAT_CLIENT_INSTALL_PATH := x"~/.local/share/Steam"
export STEAM_COMPAT_DATA_PATH := justfile_dir() + "/noita/steam-compat-data"

[working-directory: 'noita']
launch: setup
    steam-run \
        "{{steam-common}}/{{proton}}/proton" \
        waitforexitandrun \
        noita.exe \
        -- \
        -no_logo_splashes \
        -gamemode

setup:
    mkdir -p "noita/steam-compat-data"
    ln -sf "{{steam-common}}/Noita/"{*.dll,noita.exe,data} noita/
    echo none > noita/_version_hash.txt # stop release notes popup (config must have the same hash string)

savedir := "noita/steam-compat-data/pfx/drive_c/users/steamuser/AppData/LocalLow/Nolla_Games_Noita"

reset:
    rm -rf noita logger.txt
    mkdir -p "{{savedir}}/"{save_shared,save00/persistent/flags}
    touch "{{savedir}}/save00/persistent/flags/intro_has_played"
    ln config.xml "{{savedir}}/save_shared/config.xml"

