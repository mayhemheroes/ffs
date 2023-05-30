#!/bin/sh

fail() {
    echo FAILED: $1
    if [ "$MNT" ]
    then
        # cd
        rm -r "$MNT"
    fi
    exit 1
}

MNT=$(mktemp -d)

unpack --into "$MNT" ../json/object.json

cd "$MNT"
case $(ls) in
    (eyes*fingernails*human*name) ;;
    (*) fail ls;;
esac
[ "$(cat name)" = "Michael Greenberg" ] || fail name
[ "$(cat eyes)" -eq 2 ] || fail eyes
[ "$(cat fingernails)" -eq 10 ] || fail fingernails
[ "$(cat human)" = "true" ] || fail human
cd - >/dev/null 2>&1

rm -r "$MNT" || fail mount
