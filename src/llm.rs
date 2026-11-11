//! LLM access through `aisdk`. The provider, model and credential come from
//! config and the credential store.

use aisdk::core::capabilities::{TextInputSupport, TextOutputSupport};
use aisdk::core::{DynamicModel, LanguageModel, LanguageModelRequest};
use aisdk::providers::XAI;
use anyhow::{Context, bail};

use crate::auth::{self, Credential, Store};
use crate::chatgpt::ChatGpt;
use crate::config::Llm;
use crate::providers::AuthMethod;

/// Sends one prompt to the configured provider and returns the model's text.
pub async fn generate(llm: &Llm, store: &Store, system: &str, prompt: &str) -> anyhow::Result<String> {
    match (llm.provider.as_str(), llm.auth) {
        ("openai", AuthMethod::Oauth) => {
            let (access, account_id) = auth::codex::fresh(store).await?;
            request(ChatGpt::new(&llm.model, access, account_id), system, prompt).await
        }
        ("xai", AuthMethod::Oauth) => {
            let model = XAI::<DynamicModel>::builder()
                .model_name(&llm.model)
                .api_key(auth::xai::fresh(store).await?)
                .build()?;
            request(model, system, prompt).await
        }
        _ => {
            let key = match store.get(&llm.provider)? {
                Some(Credential::ApiKey { key, .. }) => Some(key),
                _ => None,
            };
            dispatch(llm, key, system, prompt).await
        }
    }
}

async fn request<M>(model: M, system: &str, prompt: &str) -> anyhow::Result<String>
where
    M: LanguageModel + TextInputSupport + TextOutputSupport,
{
    let response = LanguageModelRequest::builder()
        .model(model)
        .system(system)
        .prompt(prompt)
        .build()
        .generate_text()
        .await?;
    response.text().context("the model returned no text")
}

/// aisdk providers are distinct types with no common runtime handle, so each
/// config name is mapped to its type here. Without a stored key, a provider
/// falls back to its own environment variable (e.g. `OPENROUTER_API_KEY`).
macro_rules! providers {
    ($($name:literal => $provider:ident, $label:literal;)*) => {
        /// `(id, display name)` of every API-key provider, in menu order.
        pub const API_KEY_PROVIDERS: &[(&str, &str)] = &[$(($name, $label)),*];

        async fn dispatch(
            llm: &Llm,
            key: Option<String>,
            system: &str,
            prompt: &str,
        ) -> anyhow::Result<String> {
            match llm.provider.as_str() {
                $($name => {
                    let mut builder = aisdk::providers::$provider::<DynamicModel>::builder()
                        .model_name(&llm.model);
                    if !llm.base_url.is_empty() {
                        builder = builder.base_url(&llm.base_url);
                    }
                    if let Some(key) = key {
                        builder = builder.api_key(key);
                    }
                    let model = builder.build().with_context(|| {
                        format!("{} is not set up; run `stonks login`", $label)
                    })?;
                    request(model, system, prompt).await
                })*
                other => bail!("unknown provider: {other}"),
            }
        }
    };
}

providers! {
    "openrouter" => Openrouter, "OpenRouter";
    "openai" => OpenAI, "OpenAI";
    "anthropic" => Anthropic, "Anthropic";
    "google" => Google, "Google Gemini";
    "xai" => XAI, "xAI Grok";
    "deepseek" => Deepseek, "DeepSeek";
    "mistral" => Mistral, "Mistral";
    "groq" => Groq, "Groq";
    "togetherai" => TogetherAI, "Together AI";
    "fireworks-ai" => FireworksAi, "Fireworks AI";
    "302ai" => Ai302, "302.AI";
    "abacus" => Abacus, "Abacus";
    "aihubmix" => Aihubmix, "AIHubMix";
    "alibaba" => Alibaba, "Alibaba";
    "alibaba-cn" => AlibabaCn, "Alibaba (China)";
    "amazon-bedrock" => AmazonBedrock, "Amazon Bedrock";
    "bailing" => Bailing, "Bailing";
    "baseten" => Baseten, "Baseten";
    "berget" => Berget, "Berget.AI";
    "chutes" => Chutes, "Chutes";
    "cloudflare-ai-gateway" => CloudflareAiGateway, "Cloudflare AI Gateway";
    "cloudflare-workers-ai" => CloudflareWorkersAi, "Cloudflare Workers AI";
    "cortecs" => Cortecs, "Cortecs";
    "fastrouter" => Fastrouter, "FastRouter";
    "firmware" => Firmware, "Firmware";
    "friendli" => Friendli, "Friendli";
    "github-copilot" => GithubCopilot, "GitHub Copilot";
    "github-models" => GithubModels, "GitHub Models";
    "helicone" => Helicone, "Helicone";
    "huggingface" => Huggingface, "Hugging Face";
    "iflowcn" => Iflowcn, "iFlow";
    "inception" => Inception, "Inception";
    "inference" => Inference, "Inference";
    "io-net" => IoNet, "IO.NET";
    "jiekou" => Jiekou, "Jiekou.AI";
    "kuae-cloud-coding-plan" => KuaeCloudCodingPlan, "KUAE Cloud Coding Plan";
    "llama" => Llama, "Llama";
    "lmstudio" => Lmstudio, "LM Studio";
    "lucidquery" => Lucidquery, "LucidQuery";
    "moark" => Moark, "Moark";
    "modelscope" => Modelscope, "ModelScope";
    "moonshotai" => Moonshotai, "Moonshot AI";
    "moonshotai-cn" => MoonshotaiCn, "Moonshot AI (China)";
    "morph" => Morph, "Morph";
    "nano-gpt" => NanoGpt, "NanoGPT";
    "nebius" => Nebius, "Nebius Token Factory";
    "nova" => Nova, "Nova";
    "novita-ai" => NovitaAi, "Novita AI";
    "nvidia" => Nvidia, "Nvidia";
    "ollama-cloud" => OllamaCloud, "Ollama Cloud";
    "opencode" => Opencode, "OpenCode";
    "ovhcloud" => Ovhcloud, "OVHcloud AI Endpoints";
    "poe" => Poe, "Poe";
    "requesty" => Requesty, "Requesty";
    "scaleway" => Scaleway, "Scaleway";
    "siliconflow" => Siliconflow, "SiliconFlow";
    "siliconflow-cn" => SiliconflowCn, "SiliconFlow (China)";
    "stackit" => Stackit, "STACKIT";
    "stepfun" => Stepfun, "StepFun (China)";
    "submodel" => Submodel, "submodel";
    "synthetic" => Synthetic, "Synthetic";
    "upstage" => Upstage, "Upstage";
    "vercel" => Vercel, "Vercel AI Gateway";
    "vultr" => Vultr, "Vultr";
    "wandb" => Wandb, "Weights & Biases";
    "xiaomi" => Xiaomi, "Xiaomi";
    "zai" => Zai, "Z.AI";
    "zai-coding-plan" => ZaiCodingPlan, "Z.AI Coding Plan";
    "zenmux" => Zenmux, "ZenMux";
    "zhipuai" => Zhipuai, "Zhipu AI";
    "zhipuai-coding-plan" => ZhipuaiCodingPlan, "Zhipu AI Coding Plan";
    "openaicompatible" => OpenAICompatible, "Custom (OpenAI-compatible)";
}
