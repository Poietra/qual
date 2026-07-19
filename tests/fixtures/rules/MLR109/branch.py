from manim import *


def choose_order():
    return True


class UnknownOrder(Scene):
    def construct(self):
        follower = Dot()
        driver = Dot()
        follower.add_updater(lambda mob: mob.move_to(driver.get_center()))
        driver.add_updater(lambda mob, dt: mob.shift(RIGHT * dt))
        if choose_order():
            self.add(follower, driver)
        else:
            self.add(driver, follower)
        self.wait(2)
