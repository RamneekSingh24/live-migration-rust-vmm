# VMM-Reference:
Make sure to compile vmm-reference before starting RPC server
cd /home/col732_anirudha/production/732_backup
cargo build --release


# RPC server:
Commands to start the server.
Edit the `Rocket.toml` file to setup port and ip address.

```
cd /home/col732_anirudha/production/col732_project_webserver-master
cargo build --release
sudo ./target/debug/col732_project_webserver 

```

# debugging commands

```sudo ./target/debug/vmm-reference --kernel path=../images/base_image,starter_file="../col732_project_webserver-master/tmp/config-10" --port 11000```

```sudo ./target/debug/vmm-reference --kernel path=../images/base_image,starter_file="../col732_project_webserver-master/tmp/config-10" --port 11000 --cpu_path="cpu.txt" --memory_path="mem.txt"```

```sleep 1 && echo 0 && sleep 1 && echo 1 && sleep 1 && echo 2 && sleep 1 && echo 3 && sleep 1 && echo 4 && sleep 1 && echo 5 && sleep 1 && echo 6 && sleep 1 && echo 7 && sleep 1 && echo 8 && sleep 1 && echo 9 && sleep 1 && echo 10 && sleep 1 && echo 11 && sleep 1 && echo 12 && sleep 1 && echo 13 && sleep 1 && echo 14 && sleep 1 && echo 15 && sleep 1 && echo 16 && sleep 1 && echo 17 && sleep 1 && echo 18 && sleep 1 && echo 19 && sleep 1 && echo 20 && sleep 1 && echo 0 && sleep 1 && echo 1 && sleep 1 && echo 2 && sleep 1 && echo 3 && sleep 1 && echo 4 && sleep 1 && echo 5 && sleep 1 && echo 6 && sleep 1 && echo 7 && sleep 1 && echo 8 && sleep 1 && echo 9 && sleep 1 && echo 10 && sleep 1 && echo 11 && sleep 1 && echo 12 && sleep 1 && echo 13 && sleep 1 && echo 14 && sleep 1 && echo 15 && sleep 1 && echo 16 && sleep 1 && echo 17 && sleep 1 && echo 18 && sleep 1 && echo 19 && sleep 1 && echo 20 && sleep 1 && echo 0 && sleep 1 && echo 1 && sleep 1 && echo 2 && sleep 1 && echo 3 && sleep 1 && echo 4 && sleep 1 && echo 5 && sleep 1 && echo 6 && sleep 1 && echo 7 && sleep 1 && echo 8 && sleep 1 && echo 9 && sleep 1 && echo 10 && sleep 1 && echo 11 && sleep 1 && echo 12 && sleep 1 && echo 13 && sleep 1 && echo 14 && sleep 1 && echo 15 && sleep 1 && echo 16 && sleep 1 && echo 17 && sleep 1 && echo 18 && sleep 1 && echo 19 && sleep 1 && echo 20 && sleep 1 && echo 0 && sleep 1 && echo 1 && sleep 1 && echo 2 && sleep 1 && echo 3 && sleep 1 && echo 4 && sleep 1 && echo 5 && sleep 1 && echo 6 && sleep 1 && echo 7 && sleep 1 && echo 8 && sleep 1 && echo 9 && sleep 1 && echo 10 && sleep 1 && echo 11 && sleep 1 && echo 12 && sleep 1 && echo 13 && sleep 1 && echo 14 && sleep 1 && echo 15 && sleep 1 && echo 16 && sleep 1 && echo 17 && sleep 1 && echo 18 && sleep 1 && echo 19 && sleep 1 && echo 20```
cargo run --bin simple resume cpu.txt mem.txt 11000 true





# API specifications:

# VMM-Reference:
Make sure to compile vmm-reference before starting RPC server
cd /home/col732_anirudha/production/732_backup
cargo build --release


# RPC server:
Commands to start the server.
Edit the `Rocket.toml` file to setup port and ip address.

```
cd /home/col732_anirudha/production/col732_project_webserver-master
cargo build --release
sudo ./target/debug/col732_project_webserver 

```

# API specifications


## POST /create
{
    "cpu_snapshot_path": "cpu.txt",
    "memory_snapshot_path": "mem.txt",
    "kernel_path": "../images/bzimage-hello-busybox", 
    "resume": true,
    "tap_device": "vmtap100"
}
## POST /snapshot
{
    "cpu_snapshot_path" : "cpu.txt",
    "memory_snapshot_path" : "mem.txt",
    "rpc_port": 43791,
    "resume": false,
    "tap_device" : "vmtap100"
}



# demo

cargo run -- --kernel path=../images/bzimage-hello-busybox,starter_path="tmp/config-13" --cpu_path="cpu.txt" --memory_path="mem.txt"
sudo ./target/debug/vmm-reference --kernel path=../images/bzimage-hello-busybox,starter_file="tmp/config-13" --port 10241

