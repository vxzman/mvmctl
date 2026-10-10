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
- **原生生成器集成**：遵循 systemd generator 规范。每次执行 `systemctl daemon-reload` 时，生成器会自动扫描配置文件并即时编译出独立的 `vm-<name>.service`。
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
├── vm-rhel.service
├── vm-freebsd.service
└── multi-user.target.wants/
    ├── vm-rhel.service    (若配置 autostart: true)
    └── vm-freebsd.service (若配置 autostart: true)
       │
       ▼  (systemctl start vm-<name>)
/usr/local/bin/mvmctl run /etc/mvmctl/<name>.json
       ├─ 创建并拉起 macvtap / bridge 虚拟网卡
       ├─ 打开 /dev/tap* 与 /dev/vhost-net 继承 FD
       └─ 前台启动 QEMU 或 Cloud-Hypervisor 进程
```

---

## 🚀 核心功能特性

- **双后端无缝兼容**：
  - **QEMU**：支持 UEFI 固件启动（OVMF Code + Vars）、直接内核引导、SPICE 远程桌面、QMP 套接字优雅 ACPI 关机。
  - **Cloud-Hypervisor**：支持极速直接内核引导（vmlinuz + initrd + cmdline）、O_DIRECT 高吞吐存储、REST API 套接字优雅 ACPI 关机。
- **网络原生编排**：
  - 支持 **`macvtap`** 与 **`bridge` / `tap`** 模式。
  - 自动管理接口命名规范（`qu-<name>` / `ch-<name>`），单/多网卡索引自动分配。
  - 关机后自动回收虚拟网络设备，防止网络接口泄漏。
- **全生命周期自洽**：
  - `run`：前台拉起执行（对接 systemd `ExecStart`）。
  - `stop`：纯 Rust 实现的 QMP / HTTP API ACPI 关机协议（对接 systemd `ExecStop`）。
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
3. 创建 systemd 生成器软链接：`/etc/systemd/system-generators/mvmctl-generator -> /usr/local/bin/mvmctl`。

---

## 📝 JSON 配置文件参考

配置文件默认存放在 `/etc/mvmctl/<vm_name>.json`。

### 配置字段详解

| 字段名 | 类型 | 默认值 | 说明 |
| :--- | :--- | :--- | :--- |
| `name` | string | **必填** | 虚拟机名称（决定服务名 `vm-<name>.service`） |
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
- `mode`: `"direct"`（直接内核引导）或 `"uefi"`（UEFI 固件引导，默认）。
- **直接内核引导选项**：
  - `kernel`: 内核镜像绝对路径（例如 `/hypervisor/vm-rhel/boot/vmlinuz`）。
  - `initrd`: Initramfs 镜像绝对路径（可选）。
  - `cmdline`: 内核启动命令行参数（例如 `"console=ttyS0 root=/dev/vda rw rootwait"`）。
- **UEFI 固件引导选项**：
  - `uefi_code`: OVMF 代码固件路径（默认 `/usr/share/OVMF/OVMF_CODE_4M.ms.fd`）。
  - `uefi_vars`: NVRAM 变量文件路径（默认 `/var/lib/qemu/nvram/<name>_VARS.fd`）。

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
- `mac`: 虚拟网卡 MAC 地址（例如 `"52:54:00:23:94:3f"`，可选）。
- `name`: 自定义虚拟接口名称（可选，默认自动命名为 `qu-<name>` 或 `ch-<name>`）。

#### 4. `spice` 远程桌面配置 (QEMU)
- `enable`: 是否启用 SPICE（默认 `false`）。
- `addr`: 监听地址（默认 `"0.0.0.0"`）。
- `port`: 监听端口（默认 `5900`）。
- `password`: 访问密码（可选，留空则免密）。

---

## 💡 生产配置实战示例

### 示例 1：Cloud-Hypervisor 直接内核启动 (`/etc/mvmctl/rhel.json`)
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
sudo systemctl start vm-rhel
sudo systemctl status vm-rhel
sudo systemctl stop vm-rhel

# 3. 查看开机自启状态
systemctl is-enabled vm-rhel
```

### 2. 查看所有虚拟机概览状态
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
