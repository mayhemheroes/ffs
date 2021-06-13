#!/bin/sh

fail() {
    echo FAILED: $1
    if [ "$MNT" ]
    then
        umount "$MNT"
        rmdir "$MNT"
        rm -r "$EXP"
    fi
    exit 1
}

MNT=$(mktemp -d)
EXP=$(mktemp -d)

# generate files w/newlines
printf "Michael Greenberg\n" >"${EXP}/name"
printf "2\n"                 >"${EXP}/eyes"
printf "10\n"                >"${EXP}/fingernails"
printf "true\n"              >"${EXP}/human"

ffs --newline "$MNT" ../json/object.json &
PID=$!
sleep 1
cd "$MNT"
case $(ls) in
    (eyes*fingernails*human*name) ;;
    (*) fail ls;;
esac
diff "${EXP}/name" "${MNT}/name" || fail name
diff "${EXP}/eyes" "${MNT}/eyes" || fail eyes
diff "${EXP}/fingernails" "${MNT}/fingernails" || fail fingernails
diff "${EXP}/human" "${MNT}/human" || fail human

cd - >/dev/null 2>&1
umount "$MNT" || fail unmount
sleep 1

kill -0 $PID >/dev/null 2>&1 && fail process

rmdir "$MNT" || fail mount
rm -r "$EXP"