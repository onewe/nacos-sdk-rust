use std::sync::Arc;

use futures::Future;

use crate::api::error::Error::NamingBatchRegisterServiceFailed;
use crate::api::error::Error::NamingDeregisterServiceFailed;
use crate::api::error::Error::NamingQueryServiceFailed;
use crate::api::error::Error::NamingRegisterServiceFailed;
use crate::api::error::Error::NoAvailableServiceInstance;
use crate::api::error::Result;
use crate::api::naming::InstanceChooser;
use crate::api::naming::{AsyncFuture, NamingService, ServiceInstance};
use crate::api::props::ClientProps;

use crate::common::executor;
use crate::naming::grpc::message::request::BatchInstanceRequest;
use crate::naming::grpc::message::request::InstanceRequest;
use crate::naming::grpc::message::request::ServiceQueryRequest;
use crate::naming::grpc::message::response::BatchInstanceResponse;
use crate::naming::grpc::message::response::InstanceResponse;
use crate::naming::grpc::message::response::QueryServiceResponse;

use crate::naming::grpc::{GrpcService, GrpcServiceBuilder};

use self::chooser::RandomWeightChooser;
use self::grpc::message::GrpcMessageBuilder;
use self::grpc::message::GrpcRequestMessage;
use self::grpc::message::GrpcResponseMessage;

mod chooser;
mod grpc;

pub(self) mod constants {

    pub const LABEL_SOURCE: &str = "source";

    pub const LABEL_SOURCE_SDK: &str = "sdk";

    pub const LABEL_MODULE: &str = "module";

    pub const LABEL_MODULE_NAMING: &str = "naming";

    pub const DEFAULT_GROUP: &str = "DEFAULT_GROUP";

    pub const DEFAULT_NAMESAPCE: &str = "public";

    pub const APP_FILED: &str = "app";

    pub const DEFAULT_APP_NAME: &str = "unknown";

    pub mod request {
        pub const INSTANCE_REQUEST_REGISTER: &str = "registerInstance";
        pub const DE_REGISTER_INSTANCE: &str = "deregisterInstance";
        pub const BATCH_REGISTER_INSTANCE: &str = "batchRegisterInstance";
    }
}

pub(crate) struct NacosNamingService {
    grpc_service: Arc<GrpcService>,
    namespace: String,
    app_name: String,
}

impl NacosNamingService {
    pub(crate) fn new(client_props: ClientProps) -> Self {
        let app_name = client_props
            .app_name
            .unwrap_or_else(|| self::constants::DEFAULT_APP_NAME.to_owned());
        let mut namespace = client_props.namespace;
        if namespace.is_empty() {
            namespace = self::constants::DEFAULT_NAMESAPCE.to_owned();
        }

        let grpc_service = GrpcServiceBuilder::new()
            .address(client_props.server_addr)
            .namespace(namespace.clone())
            .client_version(client_props.client_version)
            .support_remote_connection(true)
            .support_remote_metrics(true)
            .support_delta_push(false)
            .support_remote_metric(false)
            .add_label(
                self::constants::LABEL_SOURCE.to_owned(),
                self::constants::LABEL_SOURCE_SDK.to_owned(),
            )
            .add_label(
                self::constants::LABEL_MODULE.to_owned(),
                self::constants::LABEL_MODULE_NAMING.to_owned(),
            )
            .add_labels(client_props.labels)
            .build();
        let grpc_service = Arc::new(grpc_service);
        NacosNamingService {
            grpc_service,
            namespace,
            app_name,
        }
    }

    fn request_to_server<R, P>(
        &self,
        mut request: R,
    ) -> Box<dyn Future<Output = Result<P>> + Send + Unpin + 'static>
    where
        R: GrpcRequestMessage + 'static,
        P: GrpcResponseMessage + 'static,
    {
        let request_headers = request.take_headers();
        let grpc_service = self.grpc_service.clone();

        let grpc_message = GrpcMessageBuilder::new(request)
            .header(self::constants::APP_FILED.to_owned(), self.app_name.clone())
            .headers(request_headers)
            .build();

        let task = async move {
            let ret = grpc_service.unary_call_async::<R, P>(grpc_message).await?;
            let body = ret.into_body();
            Ok(body)
        };

        Box::new(Box::pin(task))
    }

    fn instance_opt(
        &self,
        service_name: String,
        group_name: Option<String>,
        service_instance: ServiceInstance,
        r_type: String,
    ) -> Box<dyn Future<Output = Result<InstanceResponse>> + Send + Unpin + 'static> {
        let group_name = group_name.unwrap_or_else(|| self::constants::DEFAULT_GROUP.to_owned());
        let request = InstanceRequest {
            r_type,
            instance: service_instance,
            namespace: self.namespace.clone(),
            service_name,
            group_name,
            ..Default::default()
        };

        let reqeust_to_server_task =
            self.request_to_server::<InstanceRequest, InstanceResponse>(request);
        Box::new(Box::pin(async move { reqeust_to_server_task.await }))
    }

    fn batch_instances_opt(
        &self,
        service_name: String,
        group_name: Option<String>,
        service_instances: Vec<ServiceInstance>,
        r_type: String,
    ) -> Box<dyn Future<Output = Result<BatchInstanceResponse>> + Send + Unpin + 'static> {
        let group_name = group_name.unwrap_or_else(|| self::constants::DEFAULT_GROUP.to_owned());
        let request = BatchInstanceRequest {
            r_type,
            instances: service_instances,
            namespace: self.namespace.clone(),
            service_name,
            group_name,
            ..Default::default()
        };

        let reqeust_to_server_task =
            self.request_to_server::<BatchInstanceRequest, BatchInstanceResponse>(request);
        Box::new(Box::pin(async move { reqeust_to_server_task.await }))
    }
}

impl NamingService for NacosNamingService {
    fn register_service(
        &self,
        service_name: String,
        group_name: Option<String>,
        service_instance: ServiceInstance,
    ) -> Result<()> {
        let future = self.register_service_async(service_name, group_name, service_instance);
        executor::block_on(future)
    }

    fn register_service_async(
        &self,
        service_name: String,
        group_name: Option<String>,
        service_instance: ServiceInstance,
    ) -> AsyncFuture<()> {
        let instance_opt_task = self.instance_opt(
            service_name,
            group_name,
            service_instance,
            self::constants::request::INSTANCE_REQUEST_REGISTER.to_owned(),
        );

        Box::new(Box::pin(async move {
            let body = instance_opt_task.await?;
            if !body.is_success() {
                let InstanceResponse {
                    result_code,
                    error_code,
                    message,
                    ..
                } = body;
                return Err(NamingRegisterServiceFailed(
                    result_code,
                    error_code,
                    message.unwrap_or_default(),
                ));
            }

            Ok(())
        }))
    }

    fn deregister_instance(
        &self,
        service_name: String,
        group_name: Option<String>,
        service_instance: ServiceInstance,
    ) -> Result<()> {
        let future = self.deregister_instance_async(service_name, group_name, service_instance);
        executor::block_on(future)
    }

    fn deregister_instance_async(
        &self,
        service_name: String,
        group_name: Option<String>,
        service_instance: ServiceInstance,
    ) -> AsyncFuture<()> {
        let instance_opt_task = self.instance_opt(
            service_name,
            group_name,
            service_instance,
            self::constants::request::DE_REGISTER_INSTANCE.to_owned(),
        );

        Box::new(Box::pin(async move {
            let body = instance_opt_task.await?;
            if !body.is_success() {
                let InstanceResponse {
                    result_code,
                    error_code,
                    message,
                    ..
                } = body;
                return Err(NamingDeregisterServiceFailed(
                    result_code,
                    error_code,
                    message.unwrap_or_default(),
                ));
            }

            Ok(())
        }))
    }

    fn batch_register_instance(
        &self,
        service_name: String,
        group_name: Option<String>,
        service_instances: Vec<ServiceInstance>,
    ) -> Result<()> {
        let future =
            self.batch_register_instance_async(service_name, group_name, service_instances);
        executor::block_on(future)
    }

    fn batch_register_instance_async(
        &self,
        service_name: String,
        group_name: Option<String>,
        service_instances: Vec<ServiceInstance>,
    ) -> AsyncFuture<()> {
        let batch_instance_opt_task = self.batch_instances_opt(
            service_name,
            group_name,
            service_instances,
            self::constants::request::BATCH_REGISTER_INSTANCE.to_owned(),
        );

        Box::new(Box::pin(async move {
            let body = batch_instance_opt_task.await?;
            if !body.is_success() {
                let BatchInstanceResponse {
                    result_code,
                    error_code,
                    message,
                    ..
                } = body;
                return Err(NamingBatchRegisterServiceFailed(
                    result_code,
                    error_code,
                    message.unwrap_or_default(),
                ));
            }

            Ok(())
        }))
    }

    fn get_all_instances(
        &self,
        service_name: String,
        group_name: Option<String>,
        clusters: Vec<String>,
        subscribe: bool,
    ) -> Result<Vec<ServiceInstance>> {
        let future = self.get_all_instances_async(service_name, group_name, clusters, subscribe);
        executor::block_on(future)
    }

    fn get_all_instances_async(
        &self,
        service_name: String,
        group_name: Option<String>,
        clusters: Vec<String>,
        _subscribe: bool,
    ) -> AsyncFuture<Vec<ServiceInstance>> {
        //TODO add subscribe logic
        let cluster = clusters.join(",");
        let group_name = group_name.unwrap_or_else(|| self::constants::DEFAULT_GROUP.to_owned());
        let request = ServiceQueryRequest {
            cluster,
            group_name,
            healthy_only: false,
            udp_port: 0,
            namespace: self.namespace.clone(),
            service_name,
            ..Default::default()
        };
        let request_task =
            self.request_to_server::<ServiceQueryRequest, QueryServiceResponse>(request);

        Box::new(Box::pin(async move {
            let response = request_task.await?;
            if !response.is_success() {
                let QueryServiceResponse {
                    result_code,
                    error_code,
                    message,
                    ..
                } = response;
                return Err(NamingQueryServiceFailed(
                    result_code,
                    error_code,
                    message.unwrap_or_default(),
                ));
            }

            let service_info = response.service_info;
            let instances = service_info.hosts;
            Ok(instances)
        }))
    }

    fn select_instance(
        &self,
        service_name: String,
        group_name: Option<String>,
        clusters: Vec<String>,
        subscribe: bool,
        healthy: bool,
    ) -> Result<Vec<ServiceInstance>> {
        let future =
            self.select_instance_async(service_name, group_name, clusters, subscribe, healthy);
        executor::block_on(future)
    }

    fn select_instance_async(
        &self,
        service_name: String,
        group_name: Option<String>,
        clusters: Vec<String>,
        subscribe: bool,
        healthy: bool,
    ) -> AsyncFuture<Vec<ServiceInstance>> {
        let get_all_instances_task =
            self.get_all_instances_async(service_name, group_name, clusters, subscribe);

        Box::new(Box::pin(async move {
            let all_instance = get_all_instances_task.await?;
            let ret: Vec<ServiceInstance> = all_instance
                .into_iter()
                .filter(|instance| {
                    healthy == instance.healthy && instance.enabled && instance.weight > 0.0
                })
                .collect();
            Ok(ret)
        }))
    }

    fn select_one_healthy_instance(
        &self,
        service_name: String,
        group_name: Option<String>,
        clusters: Vec<String>,
        subscribe: bool,
    ) -> Result<ServiceInstance> {
        let future =
            self.select_one_healthy_instance_async(service_name, group_name, clusters, subscribe);
        executor::block_on(future)
    }

    fn select_one_healthy_instance_async(
        &self,
        service_name: String,
        group_name: Option<String>,
        clusters: Vec<String>,
        subscribe: bool,
    ) -> AsyncFuture<ServiceInstance> {
        let service_name_for_tip = service_name.clone();
        let select_task =
            self.select_instance_async(service_name, group_name, clusters, subscribe, true);

        Box::new(Box::pin(async move {
            let ret = select_task.await?;
            let chooser = RandomWeightChooser::new(service_name_for_tip.clone(), ret)?;
            let instance = chooser.choose();
            if instance.is_none() {
                return Err(NoAvailableServiceInstance(service_name_for_tip));
            }
            let instance = instance.unwrap();
            Ok(instance)
        }))
    }
}

#[cfg(test)]
pub(crate) mod tests {

    use core::time;
    use std::{collections::HashMap, thread};

    use tracing::info;

    use super::*;

    #[test]
    fn test_register_service() {
        let props = ClientProps::new().server_addr("127.0.0.1:9848");

        let mut metadata = HashMap::<String, String>::new();
        metadata.insert("netType".to_string(), "external".to_string());
        metadata.insert("version".to_string(), "2.0".to_string());

        let naming_service = NacosNamingService::new(props);
        let service_instance = ServiceInstance {
            ip: "127.0.0.1".to_string(),
            port: 9090,
            metadata,
            ..Default::default()
        };

        let collector = tracing_subscriber::fmt()
            .with_thread_names(true)
            .with_file(true)
            .with_level(true)
            .with_line_number(true)
            .with_thread_ids(true)
            .finish();

        tracing::subscriber::with_default(collector, || {
            let ret = naming_service.register_service(
                "test-service".to_string(),
                Some(constants::DEFAULT_GROUP.to_string()),
                service_instance,
            );
            info!("response. {:?}", ret);
        });

        let ten_millis = time::Duration::from_secs(100);
        thread::sleep(ten_millis);
    }

    #[test]
    fn test_register_and_deregister_service() {
        let props = ClientProps::new().server_addr("127.0.0.1:9848");

        let mut metadata = HashMap::<String, String>::new();
        metadata.insert("netType".to_string(), "external".to_string());
        metadata.insert("version".to_string(), "2.0".to_string());

        let naming_service = NacosNamingService::new(props);
        let service_instance = ServiceInstance {
            ip: "127.0.0.1".to_string(),
            port: 9090,
            metadata,
            ..Default::default()
        };

        let collector = tracing_subscriber::fmt()
            .with_thread_names(true)
            .with_file(true)
            .with_level(true)
            .with_line_number(true)
            .with_thread_ids(true)
            .finish();

        tracing::subscriber::with_default(collector, || {
            let ret = naming_service.register_service(
                "test-service".to_string(),
                Some(constants::DEFAULT_GROUP.to_string()),
                service_instance.clone(),
            );
            info!("response. {:?}", ret);

            let ten_millis = time::Duration::from_secs(10);
            thread::sleep(ten_millis);

            let ret = naming_service.deregister_instance(
                "test-service".to_string(),
                Some(constants::DEFAULT_GROUP.to_string()),
                service_instance,
            );
            info!("response. {:?}", ret);

            let ten_millis = time::Duration::from_secs(10);
            thread::sleep(ten_millis);
        });
    }

    #[test]
    fn test_batch_register_service() {
        let props = ClientProps::new().server_addr("127.0.0.1:9848");

        let mut metadata = HashMap::<String, String>::new();
        metadata.insert("netType".to_string(), "external".to_string());
        metadata.insert("version".to_string(), "2.0".to_string());

        let naming_service = NacosNamingService::new(props);
        let service_instance1 = ServiceInstance {
            ip: "127.0.0.1".to_string(),
            port: 9090,
            metadata: metadata.clone(),
            ..Default::default()
        };

        let service_instance2 = ServiceInstance {
            ip: "192.168.1.1".to_string(),
            port: 8888,
            metadata: metadata.clone(),
            ..Default::default()
        };

        let service_instance3 = ServiceInstance {
            ip: "172.0.2.1".to_string(),
            port: 6666,
            metadata: metadata.clone(),
            ..Default::default()
        };

        let instance_vec = vec![service_instance1, service_instance2, service_instance3];

        let collector = tracing_subscriber::fmt()
            .with_thread_names(true)
            .with_file(true)
            .with_level(true)
            .with_line_number(true)
            .with_thread_ids(true)
            .finish();

        tracing::subscriber::with_default(collector, || {
            let ret = naming_service.batch_register_instance(
                "test-service".to_string(),
                Some(constants::DEFAULT_GROUP.to_string()),
                instance_vec,
            );
            info!("response. {:?}", ret);

            let ten_millis = time::Duration::from_secs(10);
            thread::sleep(ten_millis);
        });
    }

    #[test]
    fn test_batch_register_service_and_query_all_instances() {
        let props = ClientProps::new().server_addr("127.0.0.1:9848");

        let mut metadata = HashMap::<String, String>::new();
        metadata.insert("netType".to_string(), "external".to_string());
        metadata.insert("version".to_string(), "2.0".to_string());

        let naming_service = NacosNamingService::new(props);
        let service_instance1 = ServiceInstance {
            ip: "127.0.0.1".to_string(),
            port: 9090,
            metadata: metadata.clone(),
            ..Default::default()
        };

        let service_instance2 = ServiceInstance {
            ip: "192.168.1.1".to_string(),
            port: 8888,
            metadata: metadata.clone(),
            ..Default::default()
        };

        let service_instance3 = ServiceInstance {
            ip: "172.0.2.1".to_string(),
            port: 6666,
            metadata: metadata.clone(),
            ..Default::default()
        };
        let instance_vec = vec![service_instance1, service_instance2, service_instance3];

        let collector = tracing_subscriber::fmt()
            .with_thread_names(true)
            .with_file(true)
            .with_level(true)
            .with_line_number(true)
            .with_thread_ids(true)
            .finish();

        tracing::subscriber::with_default(collector, || {
            let ret = naming_service.batch_register_instance(
                "test-service".to_string(),
                Some(constants::DEFAULT_GROUP.to_string()),
                instance_vec,
            );
            info!("response. {:?}", ret);

            let ten_millis = time::Duration::from_secs(10);
            thread::sleep(ten_millis);

            let all_instances = naming_service.get_all_instances(
                "test-service".to_string(),
                Some(constants::DEFAULT_GROUP.to_string()),
                Vec::default(),
                false,
            );
            info!("response. {:?}", all_instances);

            thread::sleep(ten_millis);
        });
    }

    #[test]
    fn test_select_instance() {
        let props = ClientProps::new().server_addr("127.0.0.1:9848");

        let mut metadata = HashMap::<String, String>::new();
        metadata.insert("netType".to_string(), "external".to_string());
        metadata.insert("version".to_string(), "2.0".to_string());

        let naming_service = NacosNamingService::new(props);
        let service_instance1 = ServiceInstance {
            ip: "127.0.0.1".to_string(),
            port: 9090,
            metadata: metadata.clone(),
            ..Default::default()
        };

        let service_instance2 = ServiceInstance {
            ip: "192.168.1.1".to_string(),
            port: 8888,
            metadata: metadata.clone(),
            ..Default::default()
        };

        let service_instance3 = ServiceInstance {
            ip: "172.0.2.1".to_string(),
            port: 6666,
            metadata: metadata.clone(),
            ..Default::default()
        };
        let instance_vec = vec![service_instance1, service_instance2, service_instance3];

        let collector = tracing_subscriber::fmt()
            .with_thread_names(true)
            .with_file(true)
            .with_level(true)
            .with_line_number(true)
            .with_thread_ids(true)
            .finish();

        tracing::subscriber::with_default(collector, || {
            let ret = naming_service.batch_register_instance(
                "test-service".to_string(),
                Some(constants::DEFAULT_GROUP.to_string()),
                instance_vec,
            );
            info!("response. {:?}", ret);

            let ten_millis = time::Duration::from_secs(10);
            thread::sleep(ten_millis);

            let all_instances = naming_service.select_instance(
                "test-service".to_string(),
                Some(constants::DEFAULT_GROUP.to_string()),
                Vec::default(),
                false,
                true,
            );
            info!("response. {:?}", all_instances);

            thread::sleep(ten_millis);
        });
    }

    #[test]
    fn test_select_one_healthy_instance() {
        let props = ClientProps::new().server_addr("127.0.0.1:9848");

        let mut metadata = HashMap::<String, String>::new();
        metadata.insert("netType".to_string(), "external".to_string());
        metadata.insert("version".to_string(), "2.0".to_string());

        let naming_service = NacosNamingService::new(props);
        let service_instance1 = ServiceInstance {
            ip: "127.0.0.1".to_string(),
            port: 9090,
            metadata: metadata.clone(),
            ..Default::default()
        };

        let service_instance2 = ServiceInstance {
            ip: "192.168.1.1".to_string(),
            port: 8888,
            metadata: metadata.clone(),
            ..Default::default()
        };

        let service_instance3 = ServiceInstance {
            ip: "172.0.2.1".to_string(),
            port: 6666,
            metadata: metadata.clone(),
            ..Default::default()
        };
        let instance_vec = vec![service_instance1, service_instance2, service_instance3];

        let collector = tracing_subscriber::fmt()
            .with_thread_names(true)
            .with_file(true)
            .with_level(true)
            .with_line_number(true)
            .with_thread_ids(true)
            .finish();

        tracing::subscriber::with_default(collector, || {
            let ret = naming_service.batch_register_instance(
                "test-service".to_string(),
                Some(constants::DEFAULT_GROUP.to_string()),
                instance_vec,
            );
            info!("response. {:?}", ret);

            let ten_millis = time::Duration::from_secs(10);
            thread::sleep(ten_millis);

            for _ in 0..3 {
                let all_instances = naming_service.select_one_healthy_instance(
                    "test-service".to_string(),
                    Some(constants::DEFAULT_GROUP.to_string()),
                    Vec::default(),
                    false,
                );
                info!("response. {:?}", all_instances);
            }

            thread::sleep(ten_millis);
        });
    }
}
