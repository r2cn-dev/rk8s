# 测试指南

测试 rkl 之前请先构建 dev 版本的 libbridge 和 libipam。

## 自动测试

目前已经为 rkl 的基本功能编写了集成测试，在 `project/rkl` 下运行 `cargo test --test test_cli_commands` 可运行集成测试。测试需要 root 权限才能运行。

## 手动测试

在 `project/test/bundle` 中装了 busybox 和 config.json 两个容器。你可以手动在 `project/test` 目录下创建配置文件

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: simple-container-task  
  labels:
    app: my-app 
    bundle: /home/Qiaoqia/Projects/rk8s/project/test/bundles/pause   # bundle path of pause container
spec:
  containers:
    - name: main-container1    
      image: /home/Qiaoqia/Projects/rk8s/project/test/bundles/busybox   # bundle path
      args:               #the arguments of config.json file             
        - "dd"                   
        - "if=/dev/zero"  
        - "of=/dev/null"          
      ports:
        - containerPort: 80
      resources: # resource limit
        limits:
          cpu: "500m"
          memory: "512Mi"


```

并参考 `project/rkl/README.md` 中的说明进行测试。

# Testing Guide

Before testing rkl, please build the dev version of libbridge and libipam first.

## Automated Testing

Integration tests have been written for the basic functions of rkl. You can run the integration tests by executing `cargo test --test test_cli_commands` in the `project/rkl` directory. The tests require root privileges to run.

## Manual Testing

There are two containers with busybox and config.json in `project/test/bundle`. You can manually create a configuration file in the `project/test` directory:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: simple-container-task  
  labels:
    app: my-app 
    bundle: /home/Qiaoqia/Projects/rk8s/project/rkl/test/bundles/pause   # bundle path of pause container
spec:
  containers:
    - name: main-container1    
      image: /home/Qiaoqia/Projects/rk8s/project/rkl/test/bundles/busybox   # bundle path
      args:               #the arguments of config.json file             
        - "dd"                   
        - "if=/dev/zero"  
        - "of=/dev/null"          
      ports:
        - containerPort: 80
      resources: # resource limit
        limits:
          cpu: "500m"
          memory: "512Mi"
```

Refer to the instructions in `project/rkl/README.md` for testing.