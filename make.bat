@echo off
setlocal enabledelayedexpansion

if "%1"=="" goto help
if "%1"=="help" goto help
if "%1"=="setup" goto setup
if "%1"=="dev" goto dev
if "%1"=="test" goto test
if "%1"=="fmt" goto fmt
if "%1"=="lint" goto lint
if "%1"=="build" goto build
if "%1"=="release" goto release
if "%1"=="clean" goto clean
if "%1"=="docs" goto docs
if "%1"=="docs-build" goto docs-build
if "%1"=="docker" goto docker
if "%1"=="compose" goto compose
if "%1"=="package" goto package

echo 未知命令: %1
goto help

:help
echo bili-sync 构建工具:
echo.
echo 开发命令:
echo   setup     - 设置开发环境
echo   dev       - 启动开发服务器
echo   test      - 运行测试
echo   fmt       - 格式化代码
echo   lint      - 代码检查
echo.
echo 构建命令:
echo   build     - 构建项目
echo   release   - 构建发布版本
echo   clean     - 清理构建文件
echo   package   - 打包源代码
echo.
echo 文档命令:
echo   docs      - 启动文档服务器
echo   docs-build- 构建文档
echo.
echo Docker 命令:
echo   docker    - 构建 Docker 镜像
echo   compose   - 启动 Docker Compose
echo.
echo 用法: make.bat ^<命令^>
goto end

:setup
echo 正在设置开发环境...
echo 检查 Rust 环境...
cargo --version >nul 2>&1
if errorlevel 1 (
    echo 未找到 Rust。请安装 Rust: https://rustup.rs/
    exit /b 1
)
echo Rust 环境正常

echo 检查 Node.js 环境...
node --version >nul 2>&1
if errorlevel 1 (
    echo 未找到 Node.js。请安装 Node.js: https://nodejs.org/
    exit /b 1
)
echo Node.js 环境正常

echo 安装前端依赖...
cd web
npm install
if errorlevel 1 (
    echo 安装前端依赖失败
    exit /b 1
)
echo 检查并安装 autoprefixer...
call npm list autoprefixer >nul 2>&1
if errorlevel 1 (
    echo 安装 autoprefixer...
    call npm install autoprefixer --save-dev
    if errorlevel 1 (
        echo 安装 autoprefixer 失败
        exit /b 1
    )
)
echo 前端依赖安装完成

echo 同步 SvelteKit 配置...
npx svelte-kit sync
if errorlevel 1 (
    echo SvelteKit 同步失败
    exit /b 1
)

echo 构建前端...
npm run build
if errorlevel 1 (
    echo 前端构建失败
    exit /b 1
)
cd ..
echo 前端构建完成

echo 安装 Rust 依赖...
cargo check
if errorlevel 1 (
    echo 安装 Rust 依赖失败
    exit /b 1
)
echo Rust 依赖安装完成

echo 安装文档依赖...
cd docs
npm install
if errorlevel 1 (
    echo 安装文档依赖失败
    exit /b 1
)
cd ..
echo 文档依赖安装完成

echo 开发环境设置完成!
goto end

:dev
echo 正在启动开发服务器...
echo 启动 Rust 后端...
start "Rust Backend" cmd /k "cargo run --bin bili-sync-rs"
timeout /t 2 /nobreak >nul
echo 启动 Svelte 前端...
start "Svelte Frontend" cmd /k "cd web && npm run dev"
echo 所有服务已启动!
echo 后端 API: http://localhost:12345
echo 前端 UI: http://localhost:5173
goto end

:test
echo 正在运行测试...
cargo test
if errorlevel 1 (
    echo 测试失败
    exit /b 1
) else (
    echo 所有测试通过
)
goto end

:fmt
echo 正在检查代码格式（只检查不改文件，避免全局 cargo fmt 在 Windows 上制造 CRLF 噪音）...
cargo fmt --check
echo 代码格式化完成
goto end

:lint
echo 正在检查代码...
cargo clippy -- -D warnings
goto end

:build
echo 正在构建项目...
echo [DEBUG] 开始前端构建...
cd web
if not exist "node_modules" (
    echo 安装前端依赖...
    call npm install
    if errorlevel 1 (
        echo 安装前端依赖失败
        exit /b 1
    )
)
echo [DEBUG] 检查并安装 autoprefixer...
call npm list autoprefixer >nul 2>&1
if errorlevel 1 (
    echo 安装 autoprefixer...
    call npm install autoprefixer --save-dev
    if errorlevel 1 (
        echo 安装 autoprefixer 失败
        exit /b 1
    )
)
echo [DEBUG] 同步 SvelteKit 配置...
call npx svelte-kit sync
if errorlevel 1 (
    echo SvelteKit 同步失败
    exit /b 1
)
echo [DEBUG] 执行 npm run build...
call npm run build
if errorlevel 1 (
    echo 前端构建失败
    exit /b 1
)
echo [DEBUG] 前端构建完成，返回根目录...
cd ..
echo [DEBUG] 开始 Rust 后端构建...
cargo build
if errorlevel 1 (
    echo 后端构建失败
    exit /b 1
)
echo [DEBUG] 后端构建完成
echo 项目构建完成
goto end

:release
echo 正在构建发布版本...
cd web
if not exist "node_modules" (
    echo 安装前端依赖...
    npm install
    if errorlevel 1 (
        echo 安装前端依赖失败
        exit /b 1
    )
)
echo 检查并安装 autoprefixer...
call npm list autoprefixer >nul 2>&1
if errorlevel 1 (
    echo 安装 autoprefixer...
    call npm install autoprefixer --save-dev
    if errorlevel 1 (
        echo 安装 autoprefixer 失败
        exit /b 1
    )
)
echo 同步 SvelteKit 配置...
call npx svelte-kit sync
if errorlevel 1 (
    echo SvelteKit 同步失败
    exit /b 1
)
call npm run build
if errorlevel 1 (
    echo 前端构建失败
    exit /b 1
)
cd ..
cargo build --release
if errorlevel 1 (
    echo 后端构建失败
    exit /b 1
)
echo 发布版本构建完成
goto end

:clean
echo 正在清理构建文件...
cargo clean
if exist "web\build" rmdir /s /q "web\build"
if exist "web\.svelte-kit" rmdir /s /q "web\.svelte-kit"
if exist "web\node_modules" rmdir /s /q "web\node_modules"
if exist "docs\.vitepress\dist" rmdir /s /q "docs\.vitepress\dist"
if exist "docs\node_modules" rmdir /s /q "docs\node_modules"
echo 清理完成
goto end

:package
echo 正在打包源代码...
echo 步骤 1: 清理构建文件...
call :clean

echo 步骤 2: 创建源代码包...
:: 使用 PowerShell 获取当前日期时间（格式：YYYY-MM-DD_HH-MM-SS）
for /f %%i in ('powershell -Command "Get-Date -Format 'yyyy-MM-dd_HH-mm-ss'"') do set timestamp=%%i
set packageName=bili-sync-source-%timestamp%.zip

echo 包名称: %packageName%

:: 创建临时目录
set tempDir=temp_package
if exist "%tempDir%" rmdir /s /q "%tempDir%"
mkdir "%tempDir%"

:: 复制文件
echo 包含: .github
if exist ".github" (
    xcopy /s /e /q ".github" "%tempDir%\.github\" >nul 2>&1
    if errorlevel 1 echo 警告: 复制 .github 失败
) else (
    echo 警告: 未找到 .github 文件夹
)

echo 包含: crates
if exist "crates" (
    xcopy /s /e /q "crates" "%tempDir%\crates\" >nul 2>&1
    if errorlevel 1 echo 警告: 复制 crates 失败
) else (
    echo 警告: 未找到 crates 文件夹
)

echo 包含: web
if exist "web" (
    xcopy /s /e /q "web" "%tempDir%\web\" >nul 2>&1
    if errorlevel 1 echo 警告: 复制 web 失败
) else (
    echo 警告: 未找到 web 文件夹
)

echo 包含: docs
if exist "docs" (
    xcopy /s /e /q "docs" "%tempDir%\docs\" >nul 2>&1
    if errorlevel 1 echo 警告: 复制 docs 失败
) else (
    echo 警告: 未找到 docs 文件夹
)

echo 包含: scripts
if exist "scripts" (
    xcopy /s /e /q "scripts" "%tempDir%\scripts\" >nul 2>&1
    if errorlevel 1 echo 警告: 复制 scripts 失败
) else (
    echo 警告: 未找到 scripts 文件夹
)

echo 包含: assets
if exist "assets" (
    xcopy /s /e /q "assets" "%tempDir%\assets\" >nul 2>&1
    if errorlevel 1 echo 警告: 复制 assets 失败
) else (
    echo 警告: 未找到 assets 文件夹
)

echo 包含: Cargo.toml
copy "Cargo.toml" "%tempDir%\" >nul
echo 包含: Cargo.lock
copy "Cargo.lock" "%tempDir%\" >nul
echo 包含: Dockerfile
copy "Dockerfile" "%tempDir%\" >nul
echo 包含: docker-compose.yml
copy "docker-compose.yml" "%tempDir%\" >nul
echo 包含: README.md
copy "README.md" "%tempDir%\" >nul
echo 包含: rustfmt.toml
copy "rustfmt.toml" "%tempDir%\" >nul
echo 包含: .gitignore
copy ".gitignore" "%tempDir%\" >nul
echo 包含: .dockerignore
copy ".dockerignore" "%tempDir%\" >nul
if exist "config.toml" (
    echo 包含: config.toml
    copy "config.toml" "%tempDir%\" >nul
) else (
    echo 跳过: config.toml（运行时配置，不入源码包）
)
echo 包含: make.bat
copy "make.bat" "%tempDir%\" >nul

:: 清理临时目录中的不需要项
if exist "%tempDir%\web\node_modules" rmdir /s /q "%tempDir%\web\node_modules"
if exist "%tempDir%\web\build" rmdir /s /q "%tempDir%\web\build"
if exist "%tempDir%\web\.svelte-kit" rmdir /s /q "%tempDir%\web\.svelte-kit"
if exist "%tempDir%\docs\node_modules" rmdir /s /q "%tempDir%\docs\node_modules"
if exist "%tempDir%\docs\.vitepress\dist" rmdir /s /q "%tempDir%\docs\.vitepress\dist"

:: 使用 PowerShell 创建 ZIP
echo 正在创建 ZIP 包...
powershell -Command "Compress-Archive -Path '%tempDir%\*' -DestinationPath '%packageName%' -Force"

if exist "%packageName%" (
    echo 包创建成功!
    for %%A in ("%packageName%") do (
        set /a sizeInMB=%%~zA/1024/1024
        echo 文件: %%~nxA
        echo 大小: !sizeInMB! MB
    )
) else (
    echo 创建包失败
    exit /b 1
)

:: 清理
rmdir /s /q "%tempDir%"
echo 打包完成!
goto end

:docs
echo 正在启动文档服务器...
cd docs
if not exist "node_modules" (
    echo 安装文档依赖...
    call npm install
    if errorlevel 1 (
        echo 安装文档依赖失败
        exit /b 1
    )
)
npm run docs:dev
cd ..
goto end

:docs-build
echo 正在构建文档...
cd docs
if not exist "node_modules" (
    echo 安装文档依赖...
    call npm install
    if errorlevel 1 (
        echo 安装文档依赖失败
        exit /b 1
    )
)
npm run docs:build
cd ..
echo 文档构建完成
goto end

:docker
echo 正在构建 Docker 镜像...
docker build -t bili-sync .
goto end

:compose
echo 正在启动 Docker Compose...
docker-compose up -d
goto end

:end