use std::{collections::HashMap, env};

use rkl::task::{ContainerSpec, ObjectMeta, PodSpec, PodTask, Port};

fn bundles_path(name: &str) -> String {
    let root_dir = env::current_dir().unwrap();
    root_dir
        .parent()
        .unwrap()
        .join("test/bundles/")
        .join(name)
        .to_str()
        .unwrap()
        .to_string()
}

pub fn get_pod_config<T, S>(args: Vec<S>, name: T) -> PodTask
where
    T: Into<String>,
    S: Into<String>,
{
    let name = name.into();
    let args = args.into_iter().map(Into::into).collect();
    PodTask {
        api_version: "v1".to_string(),
        kind: "Pod".to_string(),
        metadata: ObjectMeta {
            name,
            labels: HashMap::from([
                ("app".to_string(), "my-app".to_string()),
                ("bundle".to_string(), bundles_path("pause")),
            ]),
            namespace: String::new(),
            annotations: std::collections::HashMap::new(),
        },
        spec: PodSpec {
            containers: vec![ContainerSpec {
                name: "main-container1".to_string(),
                image: bundles_path("busybox"),
                args,
                ports: vec![Port {
                    container_port: 80,
                    protocol: String::new(),
                    host_ip: String::new(),
                    host_port: 0,
                }],
                resources: None,
            }],
            init_containers: vec![],
        },
    }
}
