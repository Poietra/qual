from manim import Scene, Square


class WithUpdater(Scene):
    def construct(self):
        self.always_update_mobjects = True
        square = Square()
        self.add(square)
        square.add_updater(lambda m, dt: m.rotate(dt))
        self.wait(3)


class NoFlag(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        self.wait(3)
