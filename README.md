# mvmctl (MicroVM Controller & Systemd Quadlet Generator)

`mvmctl` 是一个基于 **Rust** 开发的高性能微虚拟机（MicroVM / VM）管理工具与原生 **systemd Quadlet 风格生成器**。

---

## 📖 背景与设计理念

在传统的虚拟化运维中，通常使用类似 `qemu@<vm>.service` 或 `cloud-hypervisor@<vm>.service` 的 systemd 多实例模板，并依赖大量外挂 Bash 脚本拼接参数。这种方案存在明显痛点：
1. **多网卡与 macvtap 处理脆弱**：Bash 脚本在操作 `/dev/tap<ifindex>` 并通过 `exec 3<>/dev/tap...` 重定向文件描述符时极易出错，缺乏健壮性。
2. **缺乏独立配置管理**：模板化单元（Template Unit）难以针对不同虚拟机定制启停超时、前后置依赖、不同的硬件特性（如 SPICE 图形桌面、O_DIRECT 存储特性等）。
3. **缺少生命周期统一治理**：优雅关机（ACPI powerdown）与残存虚拟网卡、Unix Socket 的清理往往散落各处，缺乏状态自检能力。

**`mvmctl` 汲取了 Podman Quadlet 的核心架构设计**：
- **声明式配置**：通过直观的 JSON 文件（如 `/etc/mvmctl/rhel.json`、`/etc/mvmctl/freebsd.json`）定义虚拟机的全部硬件与拓扑属性。
- **原生生成器集成**：遵循 systemd generator 规范。每次执行 `systemctl daemon-reload` 时，生成器会自动扫描配置文件并即时编译出独立的 `<name>.vm.service`。
- **双后端统一调度**：一套统一的配置接口，同时兼容 **QEMU** 与 **Cloud-Hypervisor**。
- **纯 Rust 安全透传**：在 Rust 子进程派生前（`pre_exec` 阶段）直接完成 macvtap 与 vhost 文件描述符的创建、重定向与继承，优雅安全。

---

## 🏗️ 架构与工作流程

```
/etc/mvmctl/
├── rhel.json       (Cloud-Hypervisor 直接内核启动)
└── freebsd.json    (QEMU UEFI 固件 + SPICE 桌面启动)
       │
       ▼  (执行 systemctl daemon-reload)
/etc/systemd/system-generators/mvmctl-generator (动态编译)
       │
       ▼
/run/systemd/generator/
├── rhel.vm.service
├── freebsd.vm.service
└── multi-user.target.wants/
    ├── rhel.vm.service    (若配置 autostart: true)
    └── freebsd.vm.service (若配置 autostart: true)
       │
       ▼  (systemctl start <name>.vm)
/usr/local/bin/mvmctl run /etc/mvmctl/<name>.json
       ├─ 创建并拉起 macvtap / bridge 虚拟网卡
       ├─ 打开 /dev/tap* 与 /dev/vhost-net 继承 FD
       └─ 前台启动 QEMU 或 Cloud-Hypervisor 进程
```

---

## 🚀 核心功能特性

- **双后端无缝兼容**：
  - **QEMU**：支持 UEFI 固件启动（OVMF Code + Vars）、直接内核引导（`vmlinuz` / 带 PVH 的 `vmlinux`，initrd 可选）、SPICE 远程桌面、QMP 套接字优雅 ACPI 关机。
  - **Cloud-Hypervisor**：支持直接内核引导（带 PVH 的 `vmlinux` 或 `bzImage`，initrd 可选）、O_DIRECT 高吞吐存储、REST API 套接字优雅 ACPI 关机。
- **网络原生编排**：
  - 支持 **`macvtap`** 与 **`bridge` / `tap`** 模式。macvtap、主网卡和 up/down 走 netlink；tap 设备用 `/dev/net/tun` 的 `TUNSETIFF` 创建，不调用 `ip`。`bridge` / `tap` 在设备 up 之后关闭 TAP 的 TX checksum offload（TSO/GSO/GRO 一并关掉），避免网桥把未填完的 UDP 校验和送进客户机。
  - 自动管理接口命名规范（`qu-<name>` / `ch-<name>`），单/多网卡索引自动分配。
  - 关机后自动回收虚拟网络设备，防止网络接口泄漏。
- **全生命周期自洽**：
  - `run`：前台拉起执行（对接 systemd `ExecStart`）。客户机在虚拟机内重启时，`mvmctl` 保持运行；若 QEMU 或 cloud-hypervisor 因重置退出，则在原进程里重新拉起。客户机关机后以状态 0 退出，`systemctl status` 显示 inactive (dead)。`systemctl status` 只表示这个进程在不在，客户机 running/paused 仍看 `mvmctl list`。
  - `stop`：先通过 QMP / HTTP API 发 ACPI 关机。超时后强制断电（QEMU `quit`，cloud-hypervisor 先 `vm.shutdown` 再 `vmm.shutdown`）。仍未退出则返回失败，由 systemd `TimeoutStopSec` 发送 SIGKILL。
  - `clean`：自动清理网络设备与残留 Unix Socket（对接 systemd `ExecStopPost`）。
  - `list`：快速查看所有虚拟机的硬件配置、开机自启状态与当前运行状态。

---

## 🛠️ 安装与部署

### 1. 源码编译与安装
```bash
cd /home/kaishuai/project/mvmctl

# 编译 release 产物
make build

# 安装至系统（需要 root 或 sudo 权限）
sudo make install
```

`make install` 将执行以下动作：
1. 安装二进制文件至 `/usr/local/bin/mvmctl`。
2. 创建配置根目录 `/etc/mvmctl/`。
3. 若 `/etc/mvmctl/config.json` 不存在，写入默认全局配置（已存在的文件不会被覆盖）。
4. 创建 systemd 生成器软链接：`/etc/systemd/system-generators/mvmctl-generator -> /usr/local/bin/mvmctl`。

---

## ⚙️ 全局配置 `config.json`

宿主机路径、二进制位置、网卡前缀、关机超时和 cloud-hypervisor 磁盘参数放在 `/etc/mvmctl/config.json`。单台虚拟机的 CPU、磁盘、网卡仍写在各自的 JSON 里。文件不存在时使用内置默认值，和仓库里的 `examples/config.json` 一致。

用 `MVMCTL_CONFIG` 指向别的全局配置文件。`MVMCTL_CONFIG_DIR` 会插到 `config_dirs` 最前面，方便临时加一个虚拟机目录。`MVMCTL_BIN` 覆盖写进 systemd 单元的 `mvmctl` 路径。搜索目录里的 `config.json` 是全局配置，不会被当成一台虚拟机。

| 字段 | 默认值 | 说明 |
| :--- | :--- | :--- |
| `config_dirs` | `/etc/mvmctl`、`/etc/mvmctl/vms`、`/usr/lib/mvmctl` | 搜索虚拟机 JSON 的目录 |
| `runtime_dir` | `/run/mvmctl` | 控制套接字和串口套接字所在目录，权限由 `runtime_dir_mode` 决定。QEMU 使用 `<name>-qmp.sock`（`list` / `stop`）和 `<name>-qmp-events.sock`（关机事件）。cloud-hypervisor 使用 `<name>-api.sock` |
| `state_dir` | `/var/lib/mvmctl` | 解析后的执行计划 |
| `log_dir` | `/var/log/mvmctl` | `serial.mode=file` 且未写路径时的日志目录 |
| `mvmctl_bin` | `/usr/local/bin/mvmctl` | 写入单元 `ExecStart` 的二进制；路径不存在时改用当前正在运行的程序 |
| `runtime_dir_mode` | `"0700"` | 运行目录权限，八进制字符串 |
| `socket_mode` | `"0600"` | 控制套接字和串口套接字权限 |
| `stop_timeout_secs` | `45` | ACPI 关机等待秒数。超时后强制断电，仍未退出则 `stop` 返回失败 |
| `max_ifname_len` | `15` | 虚拟网卡名上限（Linux `IFNAMSIZ`） |
| `qemu.binary` | `/usr/bin/qemu-system-x86_64` | QEMU 可执行文件 |
| `qemu.uefi_code` | `/usr/share/OVMF/OVMF_CODE_4M.ms.fd` | 未在虚拟机里指定时使用的 OVMF code |
| `qemu.uefi_vars_template` | `/usr/share/OVMF/OVMF_VARS_4M.ms.fd` | NVRAM 文件不存在时从此模板复制 |
| `qemu.nvram_dir` | `/var/lib/qemu/nvram` | 默认 NVRAM 目录，文件名为 `<name>_VARS.fd` |
| `qemu.interface_prefix` | `qu-` | QEMU 网卡名前缀 |
| `qemu.machine` / `cpu` / `accel` | `q35,smm=on` / `host` / `kvm` | 传给 QEMU 的机器、CPU、加速器 |
| `cloud_hypervisor.binaries` | `/usr/local/bin` 与 `/usr/bin` 下的常见文件名 | 按顺序探测，都没有再查 `PATH` |
| `cloud_hypervisor.interface_prefix` | `ch-` | cloud-hypervisor 网卡名前缀 |
| `cloud_hypervisor.disk_num_queues` | `2` | `--disk num_queues` |
| `cloud_hypervisor.disk_queue_size` | `256` | `--disk queue_size` |
| `cloud_hypervisor.pass_image_type` | `true` | 为 false 时不传 `image_type`（旧版本会拒绝这个字段） |
| `cloud_hypervisor.default_cmdline` | `console=ttyS0 root=/dev/vda rw rootwait` | 虚拟机未写 `boot.cmdline` 时使用 |
| `cloud_hypervisor.uefi_firmware` | 空 | `boot.mode=uefi` 且虚拟机未写 `boot.uefi_code` 时的固件 |
| `systemd.unit_dir` | `/etc/systemd/system` | `generate --write` 和 `generate --all` 的默认目录 |
| `systemd.timeout_stop_sec` | `90` | 单元 `TimeoutStopSec`。实际取值不会小于 `stop_timeout_secs + 15` |
| `systemd.after` / `wants` | `network-online.target` | 写进单元 `[Unit]` |

块设备磁盘会额外生成对应的 `.device` 依赖；普通文件路径使用 `RequiresMountsFor=`。

未写 `serial` 时，QEMU 和 cloud-hypervisor 都使用 `<runtime_dir>/<name>-serial.sock`。要回到 QEMU 的标准输入输出，写成 `"serial": {"mode": "stdio"}`。

---

## 📝 JSON 配置文件参考

配置文件默认存放在 `/etc/mvmctl/<vm_name>.json`。

### 配置字段详解

| 字段名 | 类型 | 默认值 | 说明 |
| :--- | :--- | :--- | :--- |
| `name` | string | **必填** | 虚拟机名称（决定服务名 `<name>.vm.service`） |
| `description` | string | `null` | 虚拟机描述（展示在 systemd unit 描述中） |
| `backend` | string | **必填** | 后端类型：`"qemu"` 或 `"cloud-hypervisor"`（可缩写为 `"ch"`） |
| `hypervisor_path` | string | `null` | 自定义 Hypervisor 二进制绝对路径（不填则自动探测） |
| `cpus` | integer | `2` | 分配的 vCPU 核心数 |
| `memory` | string | `"2G"` | 内存大小（支持 `"1G"`、`"2048M"`、`"4G"` 等） |
| `autostart` | boolean | `false` | 是否开机自启（`true` 会自动软链接至 `multi-user.target.wants`） |
| `boot` | object | `{}` | 引导配置（详见下表） |
| `disks` | array | `[]` | 磁盘驱动列表（详见下表） |
| `networks` | array | `[]` | 网络接口列表（详见下表） |
| `spice` | object | `null` | SPICE 远程桌面配置（仅 QEMU 支持） |
| `serial` | object | `null` | 串口控制台配置 |
| `extra_args` | array | `[]` | 透传给底层 Hypervisor 的原生附加参数 |

#### 1. `boot` 引导配置
- `mode`: `"direct"`（直接内核引导）或 `"uefi"`（UEFI 固件引导，默认）。两种后端用同一组字段。
- **直接内核引导选项**（仅 `"mode": "direct"` 时生效）：
  - `kernel`: 内核镜像绝对路径。必填。
  - `initrd`: Initramfs 镜像绝对路径。可选。
  - `cmdline`: 内核启动命令行。QEMU 未写则不传 `-append`。cloud-hypervisor 未写则用全局配置里的 `cloud_hypervisor.default_cmdline`。
- **UEFI 固件引导选项**（`"mode": "uefi"` 时忽略 `kernel`、`initrd`、`cmdline`）：
  - `uefi_code`: OVMF 代码固件路径（默认 `/usr/share/OVMF/OVMF_CODE_4M.ms.fd`）。
  - `uefi_vars`: NVRAM 变量文件路径（默认 `/var/lib/qemu/nvram/<name>_VARS.fd`）。

QEMU 把这三项传成 `-kernel`、`-initrd`、`-append`。cloud-hypervisor 传成 `--kernel`、`--initramfs`、`--cmdline`。按场景怎么选，见下面的「直接内核引导」。

#### 2. `disks` 磁盘配置
- `path`: 磁盘块设备或镜像文件路径（如 `/dev/ubuntu-vg/rhel.raw` 或 `/path/to/disk.qcow2`）。
- `format`: 磁盘格式，`"raw"` 或 `"qcow2"`（默认 `"raw"`）。
- `direct`: 是否开启直接 I/O（`O_DIRECT`，Cloud-Hypervisor 支持，默认 `true`）。
- `cache`: 缓存模式（QEMU 支持，默认 `"none"`）。
- `aio`: 异步 I/O 引擎（QEMU 支持，默认 `"io_uring"`）。
- `discard`: 空间回收机制（QEMU 支持，默认 `"unmap"`）。
- `readonly`: 是否以只读方式挂载（默认 `false`）。

#### 3. `networks` 网络配置
- `mode`: 模式，`"macvtap"`（默认）或 `"bridge"`。
- `parent`: 上联物理网卡或宿主网桥名称（例如 `"enp1s0"` 或 `"brg0"`）。
- `mac`: 虚拟网卡 MAC。写成 `"52:54:00:23:94:3f"` 则原样使用。省略或 `""` 时，`mvmctl check` 和第一次 `mvmctl run` 会按虚拟机名和网卡序号从 `52:54:00` 分配，并写回这个 JSON。systemd generator 只用同一算法在内存里填地址，不会改配置文件。
- `name`: 自定义虚拟接口名称（可选，默认自动命名为 `qu-<name>` 或 `ch-<name>`）。

#### 4. `spice` 远程桌面配置 (QEMU)
- `enable`: 是否启用 SPICE（默认 `false`）。
- `addr`: 监听地址（默认 `"0.0.0.0"`）。
- `port`: 监听端口（默认 `5900`）。
- `password`: 访问密码（可选，留空则免密）。

---

## 💡 直接内核引导

`"mode": "direct"` 时两种后端都读 `kernel`、`initrd`、`cmdline`。能不能省掉 initrd，看这个内核自己能不能挂上根磁盘，和镜像是不是解压后的 `vmlinux` 无关。

### 场景 1：发行版内核，带 initrd

发行版内核通常把 virtio 和文件系统做成模块，模块在 `initrd.img` 里。`vmlinuz` 或 `bzImage` 都要带上 initrd，并在 `cmdline` 里写 `root=`。QEMU 和 cloud-hypervisor 用同一段 `boot`。

```json
"boot": {
  "mode": "direct",
  "kernel": "/hypervisor/vm-rhel/boot/vmlinuz",
  "initrd": "/hypervisor/vm-rhel/boot/initrd.img",
  "cmdline": "console=ttyS0 root=/dev/vda rw rootwait"
}
```

把发行版的 `vmlinuz` 解压成 `vmlinux` 也不会把这些模块收进内核，仍然需要 initrd。

### 场景 2：驱动编进内核，不带 initrd

磁盘驱动和根文件系统编进内核（例如 `CONFIG_VIRTIO_BLK=y` 和 `CONFIG_EXT4_FS=y`），命令行里有 `root=/dev/vda` 这类路径时，可以不写 `initrd`。Cloud Hypervisor 自带的那类内核就是这样启动的。

```json
"boot": {
  "mode": "direct",
  "kernel": "/hypervisor/vm-app/boot/vmlinux",
  "cmdline": "console=ttyS0 root=/dev/vda rw rootwait"
}
```

`backend` 写成 `"qemu"` 或 `"cloud-hypervisor"` 都可以。QEMU 没写 `cmdline` 时不会加默认参数；cloud-hypervisor 会用 `cloud_hypervisor.default_cmdline`。

### 场景 3：内核文件用哪种

| 后端 | 可以直接加载 | 解压后的 `vmlinux` |
| :--- | :--- | :--- |
| QEMU | 普通 `vmlinuz` / `bzImage` | 必须带 PVH ELF note（内核配置 `CONFIG_PVH=y`）。否则 QEMU 报 `Error loading uncompressed kernel without PVH ELF Note` |
| cloud-hypervisor（x86-64） | `bzImage`，或带 PVH 的 `vmlinux` | 同样需要 PVH。官方构建产物常见路径是 `arch/x86/boot/compressed/vmlinux.bin` |

### 场景 4：UEFI 固件

`"mode": "uefi"` 时不读 `kernel`、`initrd`、`cmdline`。QEMU 挂 OVMF code 和 vars。cloud-hypervisor 使用 `boot.uefi_code`，没有则用全局配置里的 `cloud_hypervisor.uefi_firmware`。完整例子见下面的 FreeBSD。

## 💡 生产配置实战示例

### 示例 1：Cloud-Hypervisor 直接内核启动 (`/etc/mvmctl/rhel.json`)

对应场景 1。把 `backend` 改成 `"qemu"` 后，同一段 `boot` 会传给 QEMU 的 `-kernel`、`-initrd`、`-append`。
```json
{
  "name": "rhel",
  "description": "RHEL MicroVM on Cloud-Hypervisor",
  "backend": "cloud-hypervisor",
  "cpus": 2,
  "memory": "1G",
  "autostart": true,
  "boot": {
    "mode": "direct",
    "kernel": "/hypervisor/vm-rhel/boot/vmlinuz",
    "initrd": "/hypervisor/vm-rhel/boot/initrd.img",
    "cmdline": "console=ttyS0 root=/dev/vda rw rootwait"
  },
  "disks": [
    {
      "path": "/dev/ubuntu-vg/rhel.raw",
      "format": "raw",
      "direct": true
    }
  ],
  "networks": [
    {
      "mode": "bridge",
      "parent": "brg0",
      "mac": "00:11:22:33:44:66"
    }
  ],
  "serial": {
    "mode": "socket",
    "path": "/run/mvmctl/rhel-serial.sock"
  }
}
```

### 示例 2：QEMU UEFI 固件 + SPICE 远程桌面 (`/etc/mvmctl/freebsd.json`)

对应场景 4。这里的 `boot` 只提供固件，不提供内核。
```json
{
  "name": "freebsd",
  "description": "FreeBSD VM on QEMU with UEFI and SPICE",
  "backend": "qemu",
  "cpus": 2,
  "memory": "2048M",
  "autostart": true,
  "boot": {
    "mode": "uefi",
    "uefi_code": "/usr/share/OVMF/OVMF_CODE_4M.ms.fd",
    "uefi_vars": "/var/lib/libvirt/qemu/nvram/vm08-freebsd_VARS.fd"
  },
  "disks": [
    {
      "path": "/dev/ubuntu-vg/freebsd.disk",
      "format": "raw",
      "cache": "none",
      "aio": "io_uring",
      "discard": "unmap"
    }
  ],
  "networks": [
    {
      "mode": "macvtap",
      "parent": "enp1s0",
      "mac": "52:54:00:23:94:3f"
    }
  ],
  "spice": {
    "enable": true,
    "addr": "0.0.0.0",
    "port": 5900
  }
}
```

---

## 💻 常用运维操作

### 1. Quadlet 全自动模式（日常推荐）
将 JSON 配置文件放入 `/etc/mvmctl/` 后，触发 systemd 重载即可：
```bash
# 1. 触发生成器重新扫描配置
sudo systemctl daemon-reload

# 2. 启停服务
sudo systemctl start rhel.vm
sudo systemctl status rhel.vm
sudo systemctl stop rhel.vm

# 3. 查看开机自启状态
systemctl is-enabled rhel.vm
```

### 2. 串口控制台直连 (Serial Console)
如果虚拟机配置了串口 Socket（如 `mode: "socket"`），可直接通过 `console` 指令接入交互式终端：
```bash
# 连接到 freebsd 或 rhel 的串口终端
sudo mvmctl console freebsd
# 或使用别名 attach
sudo mvmctl attach freebsd
```
- 未写 `serial` 时，两边后端都会创建 `<runtime_dir>/<name>-serial.sock`。QEMU 若要监视器和串口共用标准输入输出，设置 `"serial": {"mode": "stdio"}`。
- **快捷键**：进入后即为原生串口终端，支持全键盘交互。按下 **`Ctrl + ]`** 即可安全断开退出，不影响虚拟机后台运行。

### 3. 查看所有虚拟机概览状态
```bash
mvmctl list
```
输出示例：
```
NAME             BACKEND          CPUS       MEM      AUTOSTART  STATUS      
----------------------------------------------------------------------------
freebsd          qemu             2          2048M    yes        running     
rhel             cloud-hypervisor 2          1G       yes        running     
```

### 3. 配置合法性验证
在配置变更后，可以使用 `validate` 子命令进行快速语法与文件路径预检：
```bash
mvmctl validate /etc/mvmctl/rhel.json
```

### 4. 导出静态 Systemd Service 单元
若需要将生成的 unit 固化至 `/etc/systemd/system/`：
```bash
# 查看生成的 unit 内容
mvmctl generate /etc/mvmctl/rhel.json

# 写入指定文件
sudo mvmctl generate /etc/mvmctl/rhel.json --write

# 为 /etc/mvmctl/ 下所有配置一次性生成静态 unit
sudo mvmctl generate --all
```

### 5. 独立调试与手动执行
脱离 systemd 也可以直接使用 `mvmctl` 进行前台调试：
```bash
# 前台运行（支持打印日志到当前终端）
sudo mvmctl run /etc/mvmctl/rhel.json

# 另一个终端发送优雅关机指令
sudo mvmctl stop rhel

# 清理遗留设备与套接字
sudo mvmctl clean rhel
```

---

## 📄 许可证

本项目遵循 MIT 许可证发布。
