let help = '
    build     => to build the shit
    run       => to run the shit
    zip       => to zip the shit
'

let choice = (["build" "run" "zip"] | input list 'What to do

    build     => to build the shit
    run       => to run the shit
    zip       => to zip the shit

note (route 1 b off) <- to cancel listen to yourself

')
def main [ arg?:string] {
    match $choice {
    "build"     => {
      ^cargo build --release
    }
    "run"       => {
       ^target\x86_64-pc-windows-gnullvm\release\rustmeeter.exe run --bus-a "Speakers (Realtek(R) Audio)" --bus-b "CABLE Input" --mic "Headset Microphone (MH752)" --loopback default --captions captions.log --whistle .\cactus\whistle.cact
    }
    "zip"       => {
          let name = date now | format date "%d-%m-%Y_%H%M%S"
          let files = [
                ".cargo",
                "Cargo.lock",
                "Cargo.toml",
                "README.md",
                "crates",
                "task.nu",
                "src"]
          7z a -tzip ($name)_automatools.zip ...$files -xr!*.cact
    }

      _         => { print $help }
    }

}

