use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use flate2::write::GzEncoder;
use flate2::Compression;

fn main() {
    // 首先生成built.rs文件 (用于版本信息等)
    built::write_built_file().expect("Failed to acquire build-time information");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=BILI_SYNC_RELEASE_CHANNEL");
    track_frontend_build_output();

    // 将构建通道写入到编译期常量，供运行时区分“正式版/测试版”
    if let Ok(value) = env::var("BILI_SYNC_RELEASE_CHANNEL") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            println!("cargo:rustc-env=BILI_SYNC_RELEASE_CHANNEL_BUILT={}", trimmed);
        }
    }

    let out_dir = env::var("OUT_DIR").unwrap();
    let target = env::var("TARGET").unwrap();

    // 确定目标平台的二进制文件名
    let binary_name = if target.contains("windows") {
        "aria2c.exe"
    } else {
        "aria2c"
    };

    let binary_path = Path::new(&out_dir).join(binary_name);
    let compressed_binary_path = Path::new(&out_dir).join(format!("{binary_name}.gz"));

    // 如果压缩后的二进制文件已存在，跳过获取/压缩
    if compressed_binary_path.exists() {
        return;
    }

    // 检查是否强制禁用下载（通过环境变量 BILI_SYNC_DOWNLOAD_ARIA2=false）
    let force_disable = env::var("BILI_SYNC_DOWNLOAD_ARIA2").unwrap_or_default() == "false";

    if force_disable {
        println!("cargo:warning=环境变量设置禁用下载，创建占位文件");
        handle_download_failure(&binary_path);
    } else {
        // 默认尝试获取aria2二进制文件
        println!("cargo:warning=尝试获取aria2二进制文件");
        if let Err(e) = get_aria2_for_ci(&target, &out_dir, binary_name) {
            println!("cargo:warning=获取aria2失败: {}", e);
            handle_download_failure(&binary_path);
        }
    }

    // 生成压缩后的二进制文件，供运行时解压（减少最终产物体积）
    if let Err(e) = gzip_file(&binary_path, &compressed_binary_path) {
        panic!(
            "Failed to gzip aria2 binary: {} -> {}: {e}",
            binary_path.display(),
            compressed_binary_path.display()
        );
    }
}

fn track_frontend_build_output() {
    // 前端静态资源通过 rust-embed 编译进二进制。
    // 若仅执行 `cargo build`，Cargo 默认不会感知 `web/build` 的变化，容易出现“后端已更新但前端还是旧版”的错觉。
    // 这里显式声明依赖，让前端重新构建后能触发 Rust 重新编译。
    let candidates = [
        "../../web/build/index.html",
        "../../web/build/_app/version.json",
        "../../web/build/manifest.webmanifest",
        "../../web/build/sw.js",
        "../../web/build/registerSW.js",
    ];

    let mut any_exists = false;
    for rel in candidates {
        let path = Path::new(rel);
        if path.exists() {
            any_exists = true;
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }

    if !any_exists {
        println!("cargo:warning=未找到前端构建产物(web/build)。如需更新管理页，请先执行: cd web && npm run build");
    }
}

fn gzip_file(src: &Path, dst: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = fs::read(src)?;
    if let Some(parent) = dst.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(&bytes)?;
    let compressed = encoder.finish()?;
    fs::write(dst, compressed)?;
    Ok(())
}

fn get_aria2_for_ci(target: &str, out_dir: &str, binary_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    if target.contains("windows") {
        // Windows: 下载预编译版本
        let url = "https://github.com/aria2/aria2/releases/download/release-1.37.0/aria2-1.37.0-win-64bit-build1.zip";
        let archive_name = "aria2-1.37.0-win-64bit-build1.zip";
        download_and_extract_windows(url, archive_name, out_dir, binary_name)
    } else if target.contains("linux") {
        // Linux: 下载静态链接版本
        download_static_aria2_linux(target, out_dir, binary_name)
    } else if target.contains("darwin") {
        // macOS: 下载预编译版本
        download_aria2_macos(out_dir, binary_name)
    } else {
        // 其他平台: 回退到系统安装方式
        install_aria2_from_system(out_dir, binary_name)
    }
}

fn download_static_aria2_linux(
    target: &str,
    out_dir: &str,
    binary_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let binary_path = Path::new(out_dir).join(binary_name);

    // 使用静态链接的aria2 builds from abcfy2/aria2-static-build
    let (zip_url, zip_filename) = if target.contains("x86_64") {
        (
            "https://github.com/abcfy2/aria2-static-build/releases/download/1.37.0/aria2-x86_64-linux-musl_static.zip",
            "aria2-x86_64-linux-musl_static.zip",
        )
    } else if target.contains("aarch64") {
        (
            "https://github.com/abcfy2/aria2-static-build/releases/download/1.37.0/aria2-aarch64-linux-musl_static.zip",
            "aria2-aarch64-linux-musl_static.zip",
        )
    } else {
        println!("cargo:warning=不支持的Linux架构: {}", target);
        return install_aria2_from_system(out_dir, binary_name);
    };

    println!("cargo:warning=下载静态链接的aria2: {}", zip_url);

    let zip_path = Path::new(out_dir).join(zip_filename);

    // 下载zip文件
    let download_result = {
        #[cfg(target_os = "windows")]
        {
            download_with_powershell(zip_url, &zip_path)
        }
        #[cfg(not(target_os = "windows"))]
        {
            download_with_curl_or_wget(zip_url, &zip_path)
        }
    };

    if download_result.is_err() || !zip_path.exists() {
        println!("cargo:warning=下载失败，回退到系统安装方式");
        return install_aria2_from_system(out_dir, binary_name);
    }

    // 解压zip文件
    if let Err(e) = extract_zip(&zip_path, out_dir, binary_name) {
        println!("cargo:warning=解压失败: {}，回退到系统安装方式", e);
        let _ = std::fs::remove_file(&zip_path);
        return install_aria2_from_system(out_dir, binary_name);
    }

    if !binary_path.exists() {
        println!("cargo:warning=解压后的aria2文件不存在，回退到系统安装方式");
        let _ = std::fs::remove_file(&zip_path);
        return install_aria2_from_system(out_dir, binary_name);
    }

    // 设置可执行权限
    if let Err(e) = set_executable_permissions(&binary_path) {
        println!("cargo:warning=设置可执行权限失败: {}，回退到系统安装方式", e);
        let _ = std::fs::remove_file(&zip_path);
        return install_aria2_from_system(out_dir, binary_name);
    }

    // 验证下载的文件
    if let Ok(output) = Command::new(&binary_path).arg("--version").output() {
        let version = String::from_utf8_lossy(&output.stdout);
        println!("cargo:warning=aria2版本: {}", version.trim());
    } else {
        println!("cargo:warning=无法验证aria2版本，但继续使用");
    }

    // 清理zip文件
    let _ = std::fs::remove_file(&zip_path);

    println!("cargo:warning=成功提取静态aria2二进制文件: {}", binary_path.display());
    Ok(())
}

fn download_aria2_macos(out_dir: &str, binary_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let _binary_path = Path::new(out_dir).join(binary_name);

    // 尝试下载官方macOS预编译版本
    println!("cargo:warning=macOS检测aria2安装状态");

    // aria2官方仓库不提供独立的macOS二进制文件，只有源码
    // 最新版本1.37.0可通过以下方式获取：
    // 1. 使用Homebrew: brew install aria2
    // 2. 使用MacPorts: port install aria2
    // 3. 从源码编译
    println!("cargo:warning=aria2官方不提供macOS预编译二进制文件");
    println!("cargo:warning=将尝试通过Homebrew自动安装aria2");

    // 检查Homebrew是否存在，如果存在则尝试安装aria2
    if let Ok(output) = Command::new("brew").arg("--version").output() {
        if output.status.success() {
            println!("cargo:warning=检测到Homebrew，尝试安装aria2");

            // 检查aria2是否已安装
            if let Ok(output) = Command::new("brew").args(["list", "aria2"]).output() {
                if output.status.success() {
                    println!("cargo:warning=aria2已通过Homebrew安装");
                } else {
                    println!("cargo:warning=通过Homebrew安装aria2");
                    let install_result = Command::new("brew").args(["install", "aria2"]).status();
                    match install_result {
                        Ok(status) if status.success() => {
                            println!("cargo:warning=aria2安装成功");
                        }
                        _ => {
                            println!("cargo:warning=aria2安装失败，将使用现有系统版本");
                        }
                    }
                }
            }
        }
    } else {
        println!("cargo:warning=未检测到Homebrew，使用现有系统aria2");
    }

    install_aria2_from_system(out_dir, binary_name)
}

fn install_aria2_from_system(out_dir: &str, binary_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let binary_path = Path::new(out_dir).join(binary_name);

    println!("cargo:warning=尝试从系统包管理器安装aria2");

    // 检查aria2是否已经安装
    if let Ok(output) = Command::new("which").arg("aria2c").output() {
        if output.status.success() {
            let system_path_raw = String::from_utf8_lossy(&output.stdout);
            let system_path = system_path_raw.trim();
            println!("cargo:warning=找到系统aria2: {}", system_path);

            // 复制系统的aria2c到我们的输出目录
            if let Err(e) = fs::copy(system_path, &binary_path) {
                println!("cargo:warning=复制系统aria2失败: {}", e);

                // 如果复制失败，尝试创建符号链接或硬链接
                #[cfg(unix)]
                if std::os::unix::fs::symlink(system_path, &binary_path).is_err() {
                    // 如果符号链接也失败，创建一个简单的包装脚本
                    let wrapper_script = format!("#!/bin/bash\nexec {} \"$@\"\n", system_path);
                    fs::write(&binary_path, wrapper_script)?;
                }
            }

            // 设置可执行权限
            set_executable_permissions(&binary_path)?;

            if binary_path.exists() {
                println!("cargo:warning=成功设置aria2二进制文件: {}", binary_path.display());
                return Ok(());
            }
        }
    }

    println!("cargo:warning=系统未安装aria2，尝试安装...");

    // 检测是否在容器中（通常cross编译时没有sudo）
    let use_sudo = Command::new("sudo").arg("--version").output().is_ok();

    // 检测操作系统并安装aria2
    let install_result = if cfg!(target_os = "linux") {
        // 尝试不同的Linux包管理器
        if Command::new("apt-get").arg("--version").output().is_ok() {
            println!("cargo:warning=使用apt-get安装aria2");
            if use_sudo {
                Command::new("sudo").args(["apt-get", "update", "-y"]).status().ok();
                Command::new("sudo")
                    .args(["apt-get", "install", "-y", "aria2"])
                    .status()
            } else {
                println!("cargo:warning=容器环境，直接使用apt-get");
                Command::new("apt-get").args(["update", "-y"]).status().ok();
                Command::new("apt-get").args(["install", "-y", "aria2"]).status()
            }
        } else if Command::new("yum").arg("--version").output().is_ok() {
            println!("cargo:warning=使用yum安装aria2");
            if use_sudo {
                Command::new("sudo").args(["yum", "install", "-y", "aria2"]).status()
            } else {
                Command::new("yum").args(["install", "-y", "aria2"]).status()
            }
        } else if Command::new("dnf").arg("--version").output().is_ok() {
            println!("cargo:warning=使用dnf安装aria2");
            if use_sudo {
                Command::new("sudo").args(["dnf", "install", "-y", "aria2"]).status()
            } else {
                Command::new("dnf").args(["install", "-y", "aria2"]).status()
            }
        } else if Command::new("pacman").arg("--version").output().is_ok() {
            println!("cargo:warning=使用pacman安装aria2");
            if use_sudo {
                Command::new("sudo")
                    .args(["pacman", "-S", "--noconfirm", "aria2"])
                    .status()
            } else {
                Command::new("pacman").args(["-S", "--noconfirm", "aria2"]).status()
            }
        } else {
            println!("cargo:warning=未找到支持的包管理器");
            return Err("未找到支持的包管理器".into());
        }
    } else if cfg!(target_os = "macos") {
        // macOS使用homebrew
        if Command::new("brew").arg("--version").output().is_ok() {
            println!("cargo:warning=使用homebrew安装aria2");
            Command::new("brew").args(["install", "aria2"]).status()
        } else {
            println!("cargo:warning=macOS未安装homebrew");
            return Err("macOS未安装homebrew".into());
        }
    } else {
        println!("cargo:warning=不支持的操作系统");
        return Err("不支持的操作系统".into());
    };

    match install_result {
        Ok(status) if status.success() => {
            println!("cargo:warning=aria2安装成功");

            // 再次尝试复制已安装的aria2
            if let Ok(output) = Command::new("which").arg("aria2c").output() {
                if output.status.success() {
                    let system_path_raw = String::from_utf8_lossy(&output.stdout);
                    let system_path = system_path_raw.trim();
                    println!("cargo:warning=找到新安装的aria2: {}", system_path);

                    // 复制到我们的输出目录
                    if fs::copy(system_path, &binary_path).is_ok() {
                        set_executable_permissions(&binary_path)?;

                        if binary_path.exists() {
                            println!("cargo:warning=成功复制aria2二进制文件: {}", binary_path.display());
                            return Ok(());
                        }
                    }
                }
            }

            Err("安装成功但找不到aria2二进制文件".into())
        }
        Ok(_) => Err("aria2安装失败".into()),
        Err(e) => Err(format!("安装aria2时出错: {}", e).into()),
    }
}

fn download_and_extract_windows(
    url: &str,
    archive_name: &str,
    out_dir: &str,
    binary_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let archive_path = Path::new(out_dir).join(archive_name);
    let binary_path = Path::new(out_dir).join(binary_name);

    // 下载文件
    println!("cargo:warning=下载 {} 到 {}", url, archive_path.display());
    download_with_powershell(url, &archive_path)?;

    if !archive_path.exists() {
        return Err("下载的文件不存在".into());
    }

    // 解压文件
    println!("cargo:warning=解压 {} 到 {}", archive_path.display(), out_dir);
    extract_zip(&archive_path, out_dir, binary_name)?;

    // 删除下载的压缩包
    let _ = fs::remove_file(&archive_path);

    if binary_path.exists() {
        println!("cargo:warning=成功提取aria2二进制文件: {}", binary_path.display());
        Ok(())
    } else {
        Err("提取后的二进制文件不存在".into())
    }
}

fn download_with_powershell(url: &str, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new("powershell")
        .args([
            "-Command",
            &format!("Invoke-WebRequest -Uri '{}' -OutFile '{}'", url, output.display()),
        ])
        .status()?;

    if !status.success() {
        println!("cargo:warning=PowerShell下载失败，尝试使用curl或wget");
        return download_with_curl_or_wget(url, output);
    }

    Ok(())
}

fn download_with_curl_or_wget(url: &str, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    // 首先尝试使用curl
    if Command::new("curl").arg("--version").output().is_ok() {
        println!("cargo:warning=使用curl下载: {}", url);
        let status = Command::new("curl").args(["-L", "-o"]).arg(output).arg(url).status()?;

        if status.success() {
            return Ok(());
        }
        println!("cargo:warning=curl下载失败，尝试wget");
    }

    // 如果curl失败或不存在，尝试wget
    if Command::new("wget").arg("--version").output().is_ok() {
        println!("cargo:warning=使用wget下载: {}", url);
        let status = Command::new("wget").args(["-O"]).arg(output).arg(url).status()?;

        if status.success() {
            return Ok(());
        }
        println!("cargo:warning=wget下载失败");
    }

    Err("curl和wget都不可用或下载失败".into())
}

fn extract_zip(archive_path: &Path, out_dir: &str, binary_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "windows")]
    {
        // 在Windows上使用PowerShell解压
        let extract_script = format!(
            "Add-Type -AssemblyName System.IO.Compression.FileSystem; \
             $zip = [System.IO.Compression.ZipFile]::OpenRead('{}'); \
             $entry = $zip.Entries | Where-Object {{ $_.Name -eq '{}' }}; \
             if ($entry) {{ \
                 [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, '{}', $true); \
             }}; \
             $zip.Dispose()",
            archive_path.display(),
            binary_name,
            Path::new(out_dir).join(binary_name).display()
        );

        let status = Command::new("powershell")
            .args(["-Command", &extract_script])
            .status()?;

        if !status.success() {
            return Err("PowerShell解压失败".into());
        }

        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        // 在Linux/Unix上使用unzip命令
        let output_path = Path::new(out_dir).join(binary_name);

        // 首先尝试直接用unzip解压单个文件
        if Command::new("unzip").arg("--version").output().is_ok() {
            println!("cargo:warning=使用unzip解压: {}", archive_path.display());

            // 列出zip文件内容，找到可执行文件
            let list_output = Command::new("unzip").args(["-l"]).arg(archive_path).output()?;

            if list_output.status.success() {
                let content = String::from_utf8_lossy(&list_output.stdout);
                println!("cargo:warning=zip文件内容: {}", content);

                // 查找可能的aria2可执行文件名
                let executable_names = ["aria2c", "aria2", "aria2.exe"];
                for exe_name in &executable_names {
                    if content.contains(exe_name) {
                        println!("cargo:warning=找到可执行文件: {}", exe_name);

                        let status = Command::new("unzip")
                            .args(["-j", "-o"]) // -j: 不创建目录结构, -o: 覆盖文件
                            .arg(archive_path)
                            .arg(exe_name)
                            .args(["-d", out_dir])
                            .status()?;

                        if status.success() {
                            // 重命名到我们期望的名称
                            let extracted_path = Path::new(out_dir).join(exe_name);
                            if extracted_path.exists() && *exe_name != binary_name {
                                std::fs::rename(&extracted_path, &output_path)?;
                            }

                            if output_path.exists() {
                                println!("cargo:warning=成功解压aria2: {}", output_path.display());
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }

        // 如果unzip失败，尝试使用Rust的zip库（但我们需要确保它在build.rs中可用）
        Err("无法解压zip文件：未找到unzip命令或zip内容不匹配".into())
    }
}

fn handle_download_failure(binary_path: &Path) {
    println!("cargo:warning=创建aria2占位文件，程序会在运行时自动检测系统安装的aria2");

    // 根据平台给出安装建议
    let install_hint = if cfg!(target_os = "macos") {
        "程序可自动使用系统aria2，如需手动安装请运行: brew install aria2"
    } else if cfg!(target_os = "linux") {
        "程序可自动使用系统aria2，如需手动安装请运行: apt install aria2 或 yum install aria2"
    } else if cfg!(target_os = "windows") {
        "程序可自动使用系统aria2，如需手动安装请访问: https://aria2.github.io/"
    } else {
        "程序可自动使用系统aria2，请手动安装aria2"
    };

    println!("cargo:warning={}", install_hint);

    // 创建父目录
    if let Some(parent) = binary_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    // 创建占位文件（更友好的内容）
    let content: Vec<u8> = if binary_path.extension().unwrap_or_default() == "exe" {
        // Windows占位文件
        "@echo off\n\
            echo ===== bili-sync aria2 占位文件 =====\n\
            echo 此文件为占位文件，程序运行时会自动检测系统安装的aria2\n\
            echo 如需手动安装，请访问: https://aria2.github.io/\n\
            echo 或使用 scoop install aria2 / choco install aria2\n\
            echo ==================================\n\
            pause"
            .to_string()
            .into_bytes()
    } else {
        // Unix占位文件
        let install_cmd = if cfg!(target_os = "macos") {
            "brew install aria2"
        } else {
            "apt install aria2  # 或 yum install aria2"
        };

        format!(
            "#!/bin/bash\n\
            echo '===== bili-sync aria2 占位文件 ====='\n\
            echo '此文件为占位文件，程序运行时会自动检测系统安装的aria2'\n\
            echo '如需手动安装，请运行: {}'\n\
            echo '=================================='\n\
            read -p '按 Enter 键继续...'",
            install_cmd
        )
        .into_bytes()
    };

    if fs::write(binary_path, content).is_ok() {
        let _ = set_executable_permissions(binary_path);
        println!("cargo:warning=已创建占位文件: {}", binary_path.display());
    }
}

#[cfg(unix)]
fn set_executable_permissions(binary_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::metadata(binary_path)?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(binary_path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable_permissions(_binary_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}
