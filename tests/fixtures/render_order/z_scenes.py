from manim import Circle, Scene, Square


class ZReorder(Scene):
    def construct(self):
        a = Square()
        b = Circle()
        self.add(a, b)
        b.set_z_index(-1)
        self.wait()


class ZPoison(Scene):
    def construct(self):
        a = Square()
        b = Circle()
        self.add(a, b)
        self.wait()
        k = 2
        b.set_z_index(k)
        self.wait()
