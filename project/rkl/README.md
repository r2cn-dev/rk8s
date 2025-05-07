
  

# RKL

  

> 该项目基于Youki(https://github.com/youki-dev/youki)实现CRI接口的相应功能，目前可以创建Pod，启动Pod，删除Pod，查看容器状态。

  

## **项目结构**

  
  

```

├── Cargo.toml
├── src
│   ├── cli_commands.rs				## the file defining CLI commands
│   ├── commands					## file from Youki
│   ├── cri							## the definition of CRI Interface
│   ├── main.rs
│   ├── rootpath.rs					## file from Youki
│   └── task						## manage a pod task
└── test
    ├── bundle-file					## one bundle can be just used for one container
    └── pod_with_a_container.yaml	## a test yaml file

```

  

----------

  

### **已实现的功能**


✅ 创建Pod

./rkl create + yaml file

```

example: ./rkl create task.yaml

```

✅ 启动Pod

./rkl start +pod name

```

example: ./rkl start pod1

```

✅ 查看Pod状态

./rkl state +pod name

```

example: ./rkl state pod1

```

✅ 创建并启动Pod

./rkl run + yaml file

```

example: ./rkl run task.yaml

  

```

✅ 删除Pod

./rkl delete + pod name

```

example: ./rkl delete pod1

```
✅进入Pod中的容器

./rkl exec [OPTIONS] <POD_NAME> <CONTAINER_ID> [COMMAND]...

```

example: ./rkl exec pod1 container1 -e PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin /bin/sh

```

✅ 守护进程模式

进入守护模式后，程序会作为一个守护进程运行，监测 `/etc/rk8s/manifests` 下面的 yaml pod 配置文件，采取措施尝试让 pod 状态与其描述的一致。

日志输出在 `/var/log/rk8s/log_{timestamp}.out`

```
./rkl daemon
```

----------

  

### **项目构建与测试（需在Root下进行）**

可以直接运行 `cargo test --package rkl --test test_cli_commands` 进行测试，同样需要 root 权限并参考 `libbridge/README.md` 添加好 `netns`。运行测试前需要先构建 dev 版本的 libbridge 和 libipam。

1. **提供Bundle文件**：

```

## an example of busybox

mkdir -p rootfs

docker export $(docker create busybox) | tar -C rootfs -xvf -

```

对于Pause容器还需要提供config.json文件

在项目中已提供pause容器的bundle与config.json文件(位于test/bundle-file/pause中)

  

2. **提供yaml文件**：

目前仅支持如下yaml文件的格式

支持扩充业务容器的数量

```yaml
apiVersion: v1

kind: Pod

metadata:

name: simple-container-task

labels:

app: my-app

bundle: /home/ich/rk8s/project/rkl/test/bundle-file/pause # bundle path of pause container

spec:

containers:

- name: main-container1

image: /home/ich/rk8s/project/rkl/test/bundle-file/busybox # bundle path

args:				#the arguments of config.json file

- "sleep"

- "10000"

ports:

- containerPort: 80

```

  

3. **构建RKL可执行文件**：

在rkl目录下执行

```

cargo build

```

4. **测试项目功能**：

在/test下已提供一个用来创建两个业务容器的Pod

在测试前请修改对应bundle文件的路径

以项目中提供的yaml为例：

create：

```

./rkl create /home/ich/rk8s/project/rkl/test/pod_with_a_containers.yaml

  

PodSandbox (Pause) created: simple-container-task, pid: 32277

Container created: main-container1 (ID: main-container1)

Pod simple-container-task created successfully

```

start：

```

./rkl start simple-container-task

  

Container started: main-container1

Pod simple-container-task started successfully

```

state:

```

./rkl state simple-container-task

  

Pod: simple-container-task

PodSandbox ID: simple-container-task

{

"ociVersion": "v1.0.2",

"id": "simple-container-task",

"status": "running",

"pid": 32409,

"bundle": "/home/ich/rk8s/project/rkl/test/bundle-file/pause",

"annotations": {},

"created": "2025-03-28T04:15:44.184375363Z",

"creator": 0,

"useSystemd": false,

"cleanUpIntelRdtSubdirectory": false

}

Containers:

{

"ociVersion": "v1.0.2",

"id": "main-container1",

"status": "running",

"pid": 32411,

"bundle": "/home/ich/rk8s/project/rkl/test/bundle-file/busybox",

"annotations": {},

"created": "2025-03-28T04:15:44.236020945Z",

"creator": 0,

"useSystemd": false,

"cleanUpIntelRdtSubdirectory": false

}


```

delete:

```

./rkl delete simple-container-task

  

Container deleted: main-container1

PodSandbox deleted: simple-container-task

Pod simple-container-task deleted successfully

```

run:

```

./rkl run /home/ich/rk8s/project/rkl/test/pod_with_a_containers.yaml

  

PodSandbox (Pause) started: simple-container-task, pid: 32608

Container created: main-container1 (ID: main-container1)

Container started: main-container1

PodSandbox ID: simple-container-task

Pod simple-container-task created and started successfully

```
exec:
```

./rkl exec simple-container-task simple-container-task-main-container1 -e PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin /bin/sh

/ # ls
bin    dev    etc    home   lib    lib64  proc   root   sys    tmp    usr    var
```


  

----------

  
  
  
  

# RKL

  

> This project implements CRI interface functionality based on Youki (https://github.com/youki-dev/youki). It currently supports creating, starting, deleting Pods, and checking container states.

  

## **Project Structure**

  

```

├── Cargo.toml
├── src
│   ├── cli_commands.rs				## the file defining CLI commands
│   ├── commands					## file from Youki
│   ├── cri							## the definition of CRI Interface
│   ├── main.rs
│   ├── rootpath.rs					## file from Youki
│   └── task						## manage a pod task
└── test
    ├── bundle-file					## one bundle can be just used for one container
    └── pod_with_a_container.yaml	## a test yaml file

```

----------

  

### **Implemented Features**

  

✅ Create Pod

`./rkl create + yaml file`

`example: ./rkl create task.yaml`

  

✅ Start Pod

`./rkl start + pod name`

`example: ./rkl start pod1`

  

✅ Check Pod Status

`./rkl state + pod name`

`example: ./rkl state pod1`

  

✅ Create and Start Pod

`./rkl run + yaml file`

`example: ./rkl run task.yaml`

  

✅ Delete Pod

`./rkl delete + pod name`

`example: ./rkl delete pod1`

✅ execute Pod

./rkl exec [OPTIONS] <POD_NAME> <CONTAINER_ID> [COMMAND]...

```

example: ./rkl exec pod1 container1 -e PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin /bin/sh

```

----------

  

### **Project Build and Testing (Must Run as Root)**

  

1. **Provide Bundle Files**

```

## an example of busybox

mkdir -p rootfs

docker export $(docker create busybox) | tar -C rootfs -xvf -

```

For the Pause container, a `config.json` file must be provided.

The project already includes a bundle and `config.json` file for the Pause container (located in `test/bundle-file/pause`).

  

2. **Provide YAML Files**

Currently, only the following YAML format is supported.

The number of business containers can be increased as needed.

```yaml

apiVersion: v1

kind: Pod

metadata:

name: simple-container-task

labels:

app: my-app

bundle: /home/ich/rk8s/project/rkl/test/bundle-file/pause # bundle path of pause container

spec:

containers:

- name: main-container1

image: /home/ich/rk8s/project/rkl/test/bundle-file/busybox # bundle path

args:			#the arguments of config.json file

- "sleep"

- "10000"

ports:

- containerPort: 80

```

  

3. **Build RKL Executable**

In the `rkl` directory, run:

```

cargo build

```

4. **Test Project Functionality**

A test YAML file is available in the `/test` directory for creating a Pod with two business containers.

Before testing, update the bundle file paths in the YAML file.

For example:

create：

```

./rkl create /home/ich/rk8s/project/rkl/test/pod_with_a_containers.yaml

  

PodSandbox (Pause) created: simple-container-task, pid: 32277

Container created: main-container1 (ID: main-container1)

Pod simple-container-task created successfully

```

start：

```

./rkl start simple-container-task

  

Container started: main-container1

Pod simple-container-task started successfully

```

state:

```

./rkl state simple-container-task

  

Pod: simple-container-task

PodSandbox ID: simple-container-task

{

"ociVersion": "v1.0.2",

"id": "simple-container-task",

"status": "running",

"pid": 32409,

"bundle": "/home/ich/rk8s/project/rkl/test/bundle-file/pause",

"annotations": {},

"created": "2025-03-28T04:15:44.184375363Z",

"creator": 0,

"useSystemd": false,

"cleanUpIntelRdtSubdirectory": false

}

Containers:

{

"ociVersion": "v1.0.2",

"id": "main-container1",

"status": "running",

"pid": 32411,

"bundle": "/home/ich/rk8s/project/rkl/test/bundle-file/busybox",

"annotations": {},

"created": "2025-03-28T04:15:44.236020945Z",

"creator": 0,

"useSystemd": false,

"cleanUpIntelRdtSubdirectory": false

}

```

delete:

```

./rkl delete simple-container-task

  

Container deleted: main-container1

PodSandbox deleted: simple-container-task

Pod simple-container-task deleted successfully

```

run:

```

./rkl run /home/ich/rk8s/project/rkl/test/pod_with_a_containers.yaml

  

PodSandbox (Pause) started: simple-container-task, pid: 32608

Container created: main-container1 (ID: main-container1)

Container started: main-container1

PodSandbox ID: simple-container-task

Pod simple-container-task created and started successfully

```

exec:
```

./rkl exec simple-container-task simple-container-task-main-container1 -e PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin /bin/sh

/ # ls
bin    dev    etc    home   lib    lib64  proc   root   sys    tmp    usr    var
```