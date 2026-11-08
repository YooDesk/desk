    // YooDesk split absolute and relative uinput devices.
    // 参考原C++绝对指针/滚轮分层；保持RustDesk IPC、Enigo与现有方法签名不变。
    struct InputDevice {
        file: File,
        created: bool,
    }

    impl InputDevice {
        // 单个设备只声明一种坐标语义，避免libinput把ABS事件当相对鼠标丢弃。
        fn open() -> Result<Self> {
            Ok(Self {
                file: File::options().write(true).custom_flags(O_NONBLOCK).open("/dev/uinput")?,
                created: false,
            })
        }

        // 每个注册ioctl都检查真实返回值，不能把创建失败报告为设备可用。
        fn bit(&self, request: c_ulong, value: c_int) -> Result<()> {
            if unsafe { ioctl(self.file.as_raw_fd(), request, value) } < 0 {
                return Err(Error::last_os_error());
            }
            Ok(())
        }

        // 原生布局范围由setup_uinput提供，不生成虚构显示器或固定坐标。
        fn axis(&self, code: c_uint, range: (i32, i32)) -> Result<()> {
            if range.0 >= range.1 {
                return Err(Error::new(ErrorKind::InvalidInput, "invalid absolute pointer range"));
            }
            self.bit(UI_SET_ABSBIT, code as c_int)?;
            let setup = UinputAbsSetup {
                code: code as c_ushort,
                absinfo: InputAbsinfo { value: range.0, minimum: range.0, maximum: range.1,
                    fuzz: 0, flat: 0, resolution: 0 },
            };
            if unsafe { ioctl(self.file.as_raw_fd(), UI_ABS_SETUP, &setup) } < 0 {
                return Err(Error::last_os_error());
            }
            Ok(())
        }

        // 只有内核成功创建设备才标记created；部分初始化失败由RAII关闭fd回收。
        fn create(&mut self, name: &str) -> Result<()> {
            let mut setup = UInputSetup {
                id: InputId { bustype: BUS_VIRTUAL, vendor: 0x2222, product: 0x3333, version: 0 },
                name: [0; UINPUT_MAX_NAME_SIZE], ff_effects_max: 0,
            };
            for (target, byte) in setup.name.iter_mut().zip(name.bytes().take(UINPUT_MAX_NAME_SIZE - 1)) {
                *target = byte as c_char;
            }
            let fd = self.file.as_raw_fd();
            if unsafe { ioctl(fd, UI_DEV_SETUP, &setup) } < 0 {
                return Err(Error::last_os_error());
            }
            if unsafe { ioctl(fd, UI_DEV_CREATE) } < 0 {
                return Err(Error::last_os_error());
            }
            self.created = true;
            Ok(())
        }

        // 输入事件写到所属设备，短写与内核拒绝均向调用者返回错误。
        fn emit(&self, event_type: c_int, code: c_int, value: c_int) -> Result<()> {
            let mut event = InputEvent { time: TimeVal { tv_sec: 0, tv_usec: 0 },
                r#type: event_type as c_ushort, code: code as c_ushort, value };
            let size = size_of::<InputEvent>();
            let written = unsafe { write(self.file.as_raw_fd(), &mut event, size) };
            if written < 0 { return Err(Error::last_os_error()); }
            if written != size as c_long {
                return Err(Error::new(ErrorKind::WriteZero, "short uinput event write"));
            }
            Ok(())
        }

        // SYN必须与前面的事件属于同一个fd，否则libinput不会提交该设备的事件包。
        fn sync(&self) -> Result<()> {
            self.emit(EV_SYN, SYN_REPORT, 0)?;
            thread::sleep(Duration::from_millis(1));
            Ok(())
        }
    }

    impl Drop for InputDevice {
        // 两个设备各自销毁；更改显示范围和连接退出不会遗留虚拟设备。
        fn drop(&mut self) {
            if self.created { unsafe { ioctl(self.file.as_raw_fd(), UI_DEV_DESTROY as c_ulong); } }
        }
    }

    pub struct UInputMouseManager {
        absolute: InputDevice,
        relative: InputDevice,
    }

    impl UInputMouseManager {
        // 绝对设备只含ABS/按键；另一个相对设备承接REL移动和滚轮，保持原IPC不变。
        pub fn new(rng_x: (i32, i32), rng_y: (i32, i32)) -> Result<Self> {
            let mut absolute = InputDevice::open()?;
            absolute.bit(UI_SET_EVBIT, EV_KEY)?;
            absolute.bit(UI_SET_EVBIT, EV_ABS)?;
            absolute.bit(UI_SET_EVBIT, EV_SYN)?;
            for button in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, BTN_EXTRA, BTN_FORWARD, BTN_BACK, BTN_TASK] {
                absolute.bit(UI_SET_KEYBIT, button)?;
            }
            // UI_SET_PROPBIT(INPUT_PROP_POINTER)，与原C++绝对指针识别方式一致。
            absolute.bit(0x4004556e, 0)?;
            absolute.axis(ABS_X, rng_x)?;
            absolute.axis(ABS_Y, rng_y)?;
            absolute.create("RustDesk Absolute Pointer")?;

            let mut relative = InputDevice::open()?;
            relative.bit(UI_SET_EVBIT, EV_KEY)?;
            relative.bit(UI_SET_EVBIT, EV_REL)?;
            relative.bit(UI_SET_EVBIT, EV_SYN)?;
            // libinput相对鼠标分类要求按钮能力；实际按钮统一由绝对设备发送。
            for button in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE] { relative.bit(UI_SET_KEYBIT, button)?; }
            for axis in [REL_X, REL_Y, REL_WHEEL, REL_HWHEEL] { relative.bit(UI_SET_RELBIT, axis as c_int)?; }
            relative.bit(0x4004556e, 0)?;
            relative.create("RustDesk Relative Pointer and Wheel")?;
            thread::sleep(Duration::from_millis(300));
            Ok(Self { absolute, relative })
        }

        // 原生鼠标按钮编号不变，额外按钮也在绝对设备中声明。
        fn map_btn(button: &MouseButton) -> c_int {
            match button {
                MouseButton::Left => BTN_LEFT, MouseButton::Right => BTN_RIGHT,
                MouseButton::Middle => BTN_MIDDLE, MouseButton::Side => BTN_SIDE,
                MouseButton::Extra => BTN_EXTRA, MouseButton::Forward => BTN_FORWARD,
                MouseButton::Back => BTN_BACK, MouseButton::Task => BTN_TASK,
            }
        }

        // 绝对事件独立提交，不能通过相对移动模拟绝对坐标来掩盖设备分类错误。
        pub fn move_to(&self, x: usize, y: usize) -> Result<()> {
            self.absolute.emit(EV_ABS, ABS_X as c_int, x as c_int)?;
            self.absolute.emit(EV_ABS, ABS_Y as c_int, y as c_int)?;
            self.absolute.sync()
        }

        // 保留上游相对移动单位换算，只把事件送到独立的相对设备。
        pub fn move_relative(&self, x: i32, y: i32) -> Result<()> {
            self.relative.emit(EV_REL, REL_X as c_int, (x as f32 / 2.).ceil() as c_int)?;
            self.relative.emit(EV_REL, REL_Y as c_int, (y as f32 / 2.).ceil() as c_int)?;
            self.relative.sync()
        }

        // 按下和抬起始终使用相同设备，避免跨设备的按钮状态无法配对。
        pub fn press_button(&self, button: &MouseButton) -> Result<()> {
            self.absolute.emit(EV_KEY, Self::map_btn(button), 1)?;
            self.absolute.sync()
        }

        // 释放只影响本设备发送的按钮。
        pub fn release_button(&self, button: &MouseButton) -> Result<()> {
            self.absolute.emit(EV_KEY, Self::map_btn(button), 0)?;
            self.absolute.sync()
        }

        // 点击复用按下/抬起，保持现有调用者顺序与错误传播。
        pub fn click_button(&self, button: &MouseButton) -> Result<()> {
            self.press_button(button)?;
            self.release_button(button)
        }

        // 水平和垂直滚轮全部在REL设备提交，不污染ABS设备分类。
        pub fn scroll_wheel(&self, direction: &ScrollDirection) -> Result<()> {
            let (code, value) = match direction {
                ScrollDirection::Up => (REL_WHEEL, 1), ScrollDirection::Down => (REL_WHEEL, -1),
                ScrollDirection::Left => (REL_HWHEEL, -1), ScrollDirection::Right => (REL_HWHEEL, 1),
            };
            self.relative.emit(EV_REL, code as c_int, value)?;
            self.relative.sync()
        }
    }
}
